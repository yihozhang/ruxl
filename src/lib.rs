use std::sync::Arc;
use std::sync::Mutex;
use std::*;

type RequestResult<T> = Result<T, Exception>;

pub trait Request<T> {
    fn run(self) -> RequestResult<T>;
}

#[derive(Debug)]
pub struct FnRequest<T, F: FnOnce() -> RequestResult<T>> {
    f: F,
}

impl<T, F: FnOnce() -> RequestResult<T>> FnRequest<T, F> {
    pub fn new(f: F) -> Self {
        FnRequest { f }
    }
}

impl<T, F: FnOnce() -> RequestResult<T>> Request<T> for FnRequest<T, F> {
    fn run(self) -> RequestResult<T> {
        (self.f)()
    }
}

struct AbsRequest(Box<dyn FnOnce() + Send>);

impl AbsRequest {
    pub fn run(self) {
        (self.0)();
    }
}

impl AbsRequest {
    fn run_all(reqs: Vec<AbsRequest>) {
        use rayon::prelude::*;
        reqs.into_par_iter().for_each(|req| req.run());
        // reqs.into_iter().for_each(|req| req.run());
    }
}

#[derive(Clone, Debug)]
pub enum ExceptionType {
    Msg(String),
    HttpError(usize),
    OOM,
    Timeout,
}

#[derive(Clone, Debug)]
pub enum Exception {
    Err(ExceptionType),
    Nothing,
}

#[derive(Debug)]
enum FetchStatus<T> {
    NotFetched,
    FetchSuccess(T),
    FetchException(Exception),
}

enum ReqResult<T> {
    Done(T),
    Blocked(Vec<AbsRequest>, Fetch<T>),
    Throw(Exception),
}

pub struct Fetch<T>(Box<dyn FnOnce(Exception) -> ReqResult<T>>);

impl<T: 'static> From<ReqResult<T>> for Fetch<T> {
    fn from(req_res: ReqResult<T>) -> Self {
        Fetch(Box::new(|_res| req_res))
    }
}

impl<T: 'static + Send + fmt::Debug> Fetch<T> {
    pub fn new<R: Request<T> + 'static + Send>(request: R) -> Fetch<T> {
        Fetch(Box::new(|_res| {
            // TODO: Arc and Mutex seems unnecessary, because
            // there will only ever be two reference, and one
            // is write, one is read. These two will never be concurrent.
            let status = Arc::new(Mutex::new(FetchStatus::<T>::NotFetched));
            let modifier = status.clone();
            let abs_request = move || {
                let res = request.run();
                let mut m = modifier.lock().unwrap();
                match res {
                    Ok(res) => *m = FetchStatus::FetchSuccess(res),
                    Err(e) => *m = FetchStatus::FetchException(e),
                }
            };
            ReqResult::Blocked(
                vec![AbsRequest(Box::new(abs_request))],
                Fetch(Box::new(move |_res| {
                    let v: &mut FetchStatus<T> = &mut status.as_ref().lock().unwrap();
                    match mem::replace(v, FetchStatus::NotFetched) {
                        FetchStatus::FetchSuccess(v) => ReqResult::Done(v),
                        FetchStatus::FetchException(e) => ReqResult::Throw(e),
                        _ => unreachable!(),
                    }
                })),
            )
        }))
    }
}

pub fn throw<T: 'static>(e: Exception) -> Fetch<T> {
    Fetch(Box::new(|_res| ReqResult::Throw(e)))
}

pub fn catch<T>(f: Fetch<T>, handler: Arc<dyn Fn(ExceptionType) -> Fetch<T>>) -> Fetch<T>
where
    T: 'static,
{
    Fetch(Box::new(|res| {
        let r = f.get()(res.clone());
        match r {
            ReqResult::Done(a) => ReqResult::Done(a).into(),
            ReqResult::Blocked(br, c) => (ReqResult::Blocked(br, catch(c, handler))).into(),
            ReqResult::Throw(e) => match e {
                Exception::Err(e) => handler(e).get()(res),
                Exception::Nothing => ReqResult::Throw(e).into(),
            },
        }
    }))
}

impl<T: 'static> Fetch<T> {
    pub fn pure(a: T) -> Fetch<T> {
        Fetch(Box::new(|_| ReqResult::Done(a)))
    }

    pub fn pure_fn(f: impl FnOnce(Exception) -> T + 'static) -> Fetch<T> {
        Fetch(Box::new(|x| ReqResult::Done(f(x))))
    }

    fn get(self) -> impl FnOnce(Exception) -> ReqResult<T> {
        self.0
    }

    // TODO: make type Fetch<U, 'a> so U does not to be static
    pub fn bind<U: 'static>(self, k: impl FnOnce(T) -> Fetch<U> + 'static) -> Fetch<U> {
        // let res: &ReqResult<T> = &a.0.lock().expect("bind");
        Fetch(Box::new(|res| {
            let r = self.get()(res.clone());
            match r {
                ReqResult::Done(a) => k(a).get()(res),
                ReqResult::Blocked(br, c) => ReqResult::Blocked(br, c.bind(k)).into(),
                ReqResult::Throw(e) => ReqResult::Throw(e).into(),
            }
        }))
    }

    pub fn fmap<U: 'static>(self, f: impl FnOnce(T) -> U + 'static) -> Fetch<U> {
        Fetch(Box::new(|res| match self.get()(res) {
            ReqResult::Done(a) => ReqResult::Done(f(a)),
            ReqResult::Blocked(br, c) => ReqResult::Blocked(br, c.fmap(f)).into(),
            ReqResult::Throw(e) => ReqResult::Throw(e).into(),
        }))
    }

    pub fn run(self, handler: Arc<dyn Fn(ExceptionType) -> Fetch<T>>) -> Result<T, Exception> {
        let runner = catch(self, handler.clone());
        let r = runner.get()(Exception::Nothing);
        match r {
            ReqResult::Done(a) => Ok(a),
            ReqResult::Blocked(br, c) => {
                AbsRequest::run_all(br);
                c.run(handler)
            }
            ReqResult::Throw(e) => Err(e),
        }
    }
}

pub fn ap<T, U, F>(f: Fetch<F>, x: Fetch<T>) -> Fetch<U>
where
    T: 'static,
    U: 'static,
    F: FnOnce(T) -> U + 'static,
{
    Fetch(Box::new(|res| match (f.get()(res.clone()), x.get()(res)) {
        (ReqResult::Done(f), ReqResult::Done(x)) => ReqResult::Done(f(x)).into(),
        (ReqResult::Done(f), ReqResult::Blocked(br, c)) => ReqResult::Blocked(br, c.fmap(f)).into(),
        (ReqResult::Blocked(br, c), ReqResult::Done(x)) => {
            ReqResult::Blocked(br, ap(c, Fetch::pure(x))).into()
        }
        (ReqResult::Blocked(br1, f), ReqResult::Blocked(br2, x)) => {
            ReqResult::Blocked(vec_merge(br1, br2), ap(f, x)).into()
        }
        (ReqResult::Done(_g), ReqResult::Throw(e)) => ReqResult::Throw(e).into(),
        (ReqResult::Throw(e), _) => ReqResult::Throw(e).into(),
        (ReqResult::Blocked(br, c), ReqResult::Throw(e)) => {
            ReqResult::Blocked(br, ap(c, throw(e))).into()
        }
    }))
}

pub fn ap2<T1, T2, U, F>(f: Fetch<F>, x: Fetch<T1>, y: Fetch<T2>) -> Fetch<U>
where
    T1: 'static,
    T2: 'static,
    U: 'static,
    F: FnOnce(T1, T2) -> U + 'static,
{
    // match (f.get(), x.get(), y.get()) {
    //     (ReqResult::Done(f), ReqResult::Done(x), ReqResult::Done(y)) => {
    //         ReqResult::Done(f(x, y)).into()
    //     }
    //     (ReqResult::Done(f), ReqResult::Done(x), ReqResult::Blocked(br, y)) => {
    //         ReqResult::Blocked(br, y.fmap(|y| f(x, y))).into()
    //     }
    //     (ReqResult::Done(f), ReqResult::Blocked(br, x), ReqResult::Done(y)) => {
    //         ReqResult::Blocked(br, x.fmap(|x| f(x, y))).into()
    //     }
    //     (ReqResult::Done(f), ReqResult::Blocked(br1, x), ReqResult::Blocked(br2, y)) => {
    //         let res = ap(x.fmap(|x| |y| f(x, y)), y);
    //         ReqResult::Blocked(vec_merge(br1, br2), res).into()
    //     }
    //     (ReqResult::Blocked(br, f), ReqResult::Done(x), ReqResult::Done(y)) => {
    //         let res = ap2(f, Fetch::pure(x), Fetch::pure(y));
    //         ReqResult::Blocked(br, res).into()
    //     }
    //     (ReqResult::Blocked(br1, f), ReqResult::Done(x), ReqResult::Blocked(br2, y)) => {
    //         let res = ap2(f, Fetch::pure(x), y);
    //         ReqResult::Blocked(vec_merge(br1, br2), res).into()
    //     }
    //     (ReqResult::Blocked(br1, f), ReqResult::Blocked(br2, x), ReqResult::Done(y)) => {
    //         let res = ap2(f, x, Fetch::pure(y));
    //         ReqResult::Blocked(vec_merge(br1, br2), res).into()
    //     }
    //     (ReqResult::Blocked(br1, f), ReqResult::Blocked(br2, x), ReqResult::Blocked(br3, y)) => {
    //         let br = vec_merge(vec_merge(br1, br2), br3);
    //         ReqResult::Blocked(br, ap2(f, x, y)).into()
    //     }
    // }
    let f = f.fmap(|f| |x| |y| f(x, y));
    ap(ap(f, x), y)
}

trait Traversable<T> {
    fn sequence(self) -> Fetch<Vec<T>>;
}

fn cons_f<T: 'static>(ys: Fetch<Vec<T>>, x: Fetch<T>) -> Fetch<Vec<T>> {
    ap(
        ys.fmap(|mut ys| {
            |x| {
                ys.push(x);
                ys
            }
        }),
        x,
    )
}

// traverse f = List.foldr cons_f (pure [])
//       where cons_f x ys = liftA2 (:) (f x) ys
impl<T: 'static, V: Iterator<Item = Fetch<T>> + 'static> Traversable<T> for V {
    fn sequence(self) -> Fetch<Vec<T>> {
        let init: Fetch<Vec<T>> = Fetch::pure(Vec::new());
        self.fold(init, cons_f)
    }
}

fn vec_merge<T>(mut a: Vec<T>, mut b: Vec<T>) -> Vec<T> {
    if a.len() < b.len() {
        mem::swap(&mut a, &mut b);
    }
    a.append(&mut b);
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand;
    #[derive(Clone, Copy, Debug)]
    struct PostId(usize);
    #[derive(Debug, Clone)]
    struct Date(String);
    #[derive(Debug, Clone)]
    struct PostContent(String);
    #[derive(Debug, Clone)]
    struct PostInfo {
        id: PostId,
        date: Date,
        topic: String,
    }

    fn get_post_ids() -> Fetch<Vec<PostId>> {
        Fetch::new(FnRequest::new(|| {
            thread::sleep(time::Duration::from_millis(500));
            Ok(vec![PostId(1), PostId(2)])
        }))
    }

    fn get_post_info(id: PostId) -> Fetch<PostInfo> {
        Fetch::new(FnRequest::new(move || {
            thread::sleep(time::Duration::from_millis(500));
            Ok(PostInfo {
                id,
                date: Date("today".to_string()),
                topic: ["Hello", "world"][id.0 % 2].to_string(),
            })
        }))
    }

    fn get_post_content(id: PostId) -> Fetch<PostContent> {
        Fetch::new(FnRequest::new(move || {
            thread::sleep(time::Duration::from_millis(500));
            Ok(PostContent(format!("A post with id {}", id.0)))
        }))
    }

    fn render_posts(it: impl Iterator<Item = (PostInfo, PostContent)>) -> String {
        it.map(|(info, content)| format!("<p>{} {}</p>", info.topic, content.0))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_side_pane(posts: String) -> impl FnOnce(String) -> String {
        move |topics| {
            format!(
                "<div class=\"topics\">{}</div>\n<div class=\"posts\">{}</div>",
                topics, posts
            )
        }
    }

    fn render_page(side: String) -> impl Fn(String) -> String {
        move |main| {
            format!(
                "<html><body><div>{}</div><div>{}</div></body></html>",
                side, main
            )
        }
    }

    fn popular_posts() -> Fetch<String> {
        Fetch::new(FnRequest::new(move || {
            thread::sleep(time::Duration::from_millis(500));
            Ok("<p>popular post 1, popular post 2, ...</p>\n".to_string())
        }))
    }

    fn topics() -> Fetch<String> {
        Fetch::new(FnRequest::new(move || {
            thread::sleep(time::Duration::from_millis(500));
            Ok("<p>topic 1, topic 2, ...</p>\n".to_string())
        }))
    }

    fn left_pane() -> Fetch<String> {
        let f = ap(Fetch::pure(render_side_pane), popular_posts());
        ap(f, topics())
    }

    fn get_all_post_info() -> Fetch<Vec<PostInfo>> {
        get_post_ids().bind(|ids| ids.into_iter().map(|id| get_post_info(id)).sequence())
    }

    fn random_crash_page() -> Fetch<String> {
        Fetch::new(FnRequest::new(move || {
            if !rand::random::<bool>() {
                Err(Exception::Err(ExceptionType::Msg(
                    "Intended error :P".to_string(),
                )))
            } else {
                Ok("".to_string())
            }
        }))
    }

    fn main_pane() -> Fetch<String> {
        get_all_post_info().bind(|posts| {
            let posts2 = posts.clone();
            posts
                .into_iter()
                .map(|post| post.id)
                .map(|id| get_post_content(id))
                .sequence()
                .bind(|content| {
                    let rendered = render_posts(posts2.into_iter().zip(content.into_iter()));
                    Fetch::<String>::pure(rendered)
                })
        })
    }

    fn error_page(e: ExceptionType) -> Fetch<String> {
        match e {
            ExceptionType::HttpError(err_code) => {
                Fetch::pure(format!("<h1> HttpError: {}", err_code))
            }
            ExceptionType::Msg(msg) => Fetch::pure(format!(
                "An error occured ... but you received this message: {}",
                msg
            )),
            ExceptionType::OOM => Fetch::pure(format!("Ooof! Out Of Memory!")),
            ExceptionType::Timeout => Fetch::pure(format!("TvT Timeout!")),
        }
    }

    fn blog() -> Fetch<String> {
        ap(ap(Fetch::pure(render_page), left_pane()), main_pane())
    }

    fn blog_with_crash() -> Fetch<String> {
        ap(
            ap(
                ap(
                    Fetch::pure(|x| |y| |z| format!("{}<br>{}", z, render_page(y)(x))),
                    left_pane(),
                ),
                main_pane(),
            ),
            random_crash_page(),
        )
    }

    #[test]
    fn run_blog() {
        let start_time = time::Instant::now();
        let blog = blog();
        println!("{}", start_time.elapsed().as_millis());
        let _result = blog.run(Arc::new(|e| error_page(e)));
        println!("{}", start_time.elapsed().as_millis());
    }
    #[test]
    fn test_random_crash() {
        match blog_with_crash().run(Arc::new(|e| error_page(e))) {
            Ok(result) => println!("{}", result),
            Err(e) => println!("{:?}", e),
        }
    }
}
