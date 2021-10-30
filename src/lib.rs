use std::sync::Arc;
use std::sync::Mutex;
use std::*;

pub trait Request<T, E = Impossible> {
    fn run(self) -> Result<T, E>;
}

#[derive(Debug)]
pub struct FnRequest<T, F: FnOnce() -> Result<T, E>, E = Impossible> {
    f: F,
}

pub enum Impossible {}

impl<T, F: FnOnce() -> Result<T, E>, E> FnRequest<T, F, E> {
    pub fn new(f: F) -> Self {
        FnRequest { f }
    }
}

impl<T, F: FnOnce() -> Result<T, E>, E> Request<T, E> for FnRequest<T, F, E> {
    fn run(self) -> Result<T, E> {
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
pub enum Exception {
    Msg(String),
    HttpError(usize),
    OutOfMemory,
    Timeout,
}

#[derive(Debug)]
enum FetchStatus<T, E = Impossible> {
    NotFetched,
    FetchSuccess(T),
    FetchException(E),
}

enum ReqResult<T, E> {
    Done(T),
    Blocked(Vec<AbsRequest>, Fetch<T, E>),
    Throw(E),
}

pub struct Fetch<T, E = Impossible>(Box<dyn FnOnce() -> ReqResult<T, E>>);

impl<T: 'static, E: 'static> From<ReqResult<T, E>> for Fetch<T, E> {
    fn from(req_res: ReqResult<T, E>) -> Self {
        Fetch(Box::new(|| req_res))
    }
}

impl<T: 'static> Fetch<T, Impossible> {
    pub fn into<E: 'static>(self) -> Fetch<T, E> {
        Fetch(Box::new(|| match self.get()() {
            ReqResult::Done(a) => ReqResult::Done(a),
            ReqResult::Blocked(br, c) => ReqResult::Blocked(br, c.into()),
            ReqResult::Throw(e) => match e {},
        }))
    }
}

impl<T: 'static + Send + fmt::Debug, E: Send + 'static> Fetch<T, E> {
    pub fn new<R: Request<T, E> + 'static + Send>(request: R) -> Fetch<T, E> {
        Fetch(Box::new(|| {
            // TODO: Arc and Mutex seems unnecessary, because
            // there will only ever be two reference, and one
            // is write, one is read. These two will never be concurrent.
            let status = Arc::new(Mutex::new(FetchStatus::<T, E>::NotFetched));
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
                Fetch(Box::new(move || {
                    let v: &mut FetchStatus<T, E> = &mut status.as_ref().lock().unwrap();
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

pub fn throw<T: 'static, E: 'static>(e: E) -> Fetch<T, E> {
    Fetch(Box::new(|| ReqResult::Throw(e)))
}

pub fn catch<T, F, E1, E2>(f: Fetch<T, E1>, handler: F) -> Fetch<T, E2>
where
    T: 'static,
    E1: 'static,
    E2: 'static,
    F: Fn(E1) -> Fetch<T, E2> + 'static,
{
    Fetch(Box::new(|| {
        let r = f.get()();
        match r {
            ReqResult::Done(a) => ReqResult::Done(a),
            ReqResult::Blocked(br, c) => ReqResult::Blocked(br, catch(c, handler)),
            ReqResult::Throw(e) => handler(e).get()(),
        }
    }))
}

impl<T: 'static, E: 'static> Fetch<T, E> {
    pub fn pure(a: T) -> Fetch<T, E> {
        Fetch(Box::new(|| ReqResult::Done(a)))
    }

    pub fn pure_fn(f: impl FnOnce() -> T + 'static) -> Fetch<T, E> {
        Fetch(Box::new(|| ReqResult::Done(f())))
    }

    fn get(self) -> impl FnOnce() -> ReqResult<T, E> {
        self.0
    }

    // TODO: make type Fetch<U, 'a> so U does not to be static
    pub fn bind<U: 'static>(self, k: impl FnOnce(T) -> Fetch<U, E> + 'static) -> Fetch<U, E> {
        // let res: &ReqResult<T> = &a.0.lock().expect("bind");
        Fetch(Box::new(|| {
            let r = self.get()();
            match r {
                ReqResult::Done(a) => k(a).get()(),
                ReqResult::Blocked(br, c) => ReqResult::Blocked(br, c.bind(k)),
                ReqResult::Throw(e) => ReqResult::Throw(e),
            }
        }))
    }

    pub fn fmap<U: 'static>(self, f: impl FnOnce(T) -> U + 'static) -> Fetch<U, E> {
        Fetch(Box::new(|| match self.get()() {
            ReqResult::Done(a) => ReqResult::Done(f(a)),
            ReqResult::Blocked(br, c) => ReqResult::Blocked(br, c.fmap(f)),
            ReqResult::Throw(e) => ReqResult::Throw(e),
        }))
    }

    pub fn run(self) -> Result<T, E> {
        match self.get()() {
            ReqResult::Done(a) => Ok(a),
            ReqResult::Blocked(br, c) => {
                AbsRequest::run_all(br);
                c.run()
            }
            ReqResult::Throw(e) => Err(e),
        }
    }
}

pub fn ap<T, U, F, E>(f: Fetch<F, E>, x: Fetch<T, E>) -> Fetch<U, E>
where
    T: 'static,
    E: 'static,
    U: 'static,
    F: FnOnce(T) -> U + 'static,
{
    Fetch(Box::new(|| match (f.get()(), x.get()()) {
        (ReqResult::Done(f), ReqResult::Done(x)) => ReqResult::Done(f(x)),
        (ReqResult::Done(f), ReqResult::Blocked(br, c)) => ReqResult::Blocked(br, c.fmap(f)),
        (ReqResult::Blocked(br, c), ReqResult::Done(x)) => {
            ReqResult::Blocked(br, ap(c, Fetch::pure(x)))
        }
        (ReqResult::Blocked(br1, f), ReqResult::Blocked(br2, x)) => {
            ReqResult::Blocked(vec_merge(br1, br2), ap(f, x))
        }
        (ReqResult::Done(_g), ReqResult::Throw(e)) => ReqResult::Throw(e),
        (ReqResult::Throw(e), _) => ReqResult::Throw(e),
        (ReqResult::Blocked(br, c), ReqResult::Throw(e)) => ReqResult::Blocked(br, ap(c, throw(e))),
    }))
}

#[allow(unused_macros)]
macro_rules! ap_builder {
    ($func:ident; $F:ident; $U:ident; $($T:ident),+; $f:ident; $($x:ident),+) => {
        pub fn $func<
            E:'static,
            $($T:'static),+,
            U:'static,
            F:FnOnce($($T),+) -> $U + 'static
        >($f: crate::Fetch<$F,E>, $($x:crate::Fetch<$T,E>),+) -> crate::Fetch<$U,E> {
            ap_builder!(@fmap $f, $($x),+);
            ap_builder!(@ap $f; $($x),+)
        }
    };
    (@fmap $f:ident, $($x:ident),+) => {
        let $f = $f.fmap(|$f| ap_builder!(@fmap_lambda $($x),+; $f, $($x),+));
    };
    (@fmap_lambda $x:ident; $f:ident, $($args:ident),+) => {
        |$x| $f($($args),+)
    };
    (@fmap_lambda $x:ident, $($xs:ident),*; $($args:ident),+) => {
        |$x| ap_builder!(@fmap_lambda $($xs),*; $($args),+)
    };
    (@ap $f:expr; $x:ident, $($xs:ident),*) => {
        ap_builder!(@ap crate::ap($f, $x); $($xs),*)
    };
    (@ap $f:expr; $x:ident) => {
        crate::ap($f, $x)
    };
}

ap_builder!(ap2; F; U; T1, T2; f; x1, x2);
ap_builder!(ap3; F; U; T1, T2, T3; f; x1, x2, x3);
ap_builder!(ap4; F; U; T1, T2, T3, T4; f; x1, x2, x3, x4);
ap_builder!(ap5; F; U; T1, T2, T3, T4, T5; f; x1, x2, x3, x4, x5);

// pub fn ap2<T1, T2, U, F>(f: Fetch<F>, x: Fetch<T1>, y: Fetch<T2>) -> Fetch<U>
// where
//     T1: 'static,
//     T2: 'static,
//     U: 'static,
//     F: FnOnce(T1, T2) -> U + 'static,
// {
//     // match (f.get(), x.get(), y.get()) {
//     //     (ReqResult::Done(f), ReqResult::Done(x), ReqResult::Done(y)) => {
//     //         ReqResult::Done(f(x, y)).into()
//     //     }
//     //     (ReqResult::Done(f), ReqResult::Done(x), ReqResult::Blocked(br, y)) => {
//     //         ReqResult::Blocked(br, y.fmap(|y| f(x, y))).into()
//     //     }
//     //     (ReqResult::Done(f), ReqResult::Blocked(br, x), ReqResult::Done(y)) => {
//     //         ReqResult::Blocked(br, x.fmap(|x| f(x, y))).into()
//     //     }
//     //     (ReqResult::Done(f), ReqResult::Blocked(br1, x), ReqResult::Blocked(br2, y)) => {
//     //         let res = ap(x.fmap(|x| |y| f(x, y)), y);
//     //         ReqResult::Blocked(vec_merge(br1, br2), res).into()
//     //     }
//     //     (ReqResult::Blocked(br, f), ReqResult::Done(x), ReqResult::Done(y)) => {
//     //         let res = ap2(f, Fetch::pure(x), Fetch::pure(y));
//     //         ReqResult::Blocked(br, res).into()
//     //     }
//     //     (ReqResult::Blocked(br1, f), ReqResult::Done(x), ReqResult::Blocked(br2, y)) => {
//     //         let res = ap2(f, Fetch::pure(x), y);
//     //         ReqResult::Blocked(vec_merge(br1, br2), res).into()
//     //     }
//     //     (ReqResult::Blocked(br1, f), ReqResult::Blocked(br2, x), ReqResult::Done(y)) => {
//     //         let res = ap2(f, x, Fetch::pure(y));
//     //         ReqResult::Blocked(vec_merge(br1, br2), res).into()
//     //     }
//     //     (ReqResult::Blocked(br1, f), ReqResult::Blocked(br2, x), ReqResult::Blocked(br3, y)) => {
//     //         let br = vec_merge(vec_merge(br1, br2), br3);
//     //         ReqResult::Blocked(br, ap2(f, x, y)).into()
//     //     }
//     // }
//     let f = f.fmap(|f| |x| |y| f(x, y));
//     ap(ap(f, x), y)
// }

trait Traversable<T, E> {
    fn sequence(self) -> Fetch<Vec<T>, E>;
}

fn cons_f<T: 'static, E: 'static>(ys: Fetch<Vec<T>, E>, x: Fetch<T, E>) -> Fetch<Vec<T>, E> {
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

impl<T: 'static, E: 'static, V: Iterator<Item = Fetch<T, E>> + 'static> Traversable<T, E> for V {
    fn sequence(self) -> Fetch<Vec<T>, E> {
        let init = Fetch::pure(Vec::new());
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

    fn get_post_content(id: PostId) -> Fetch<PostContent, Impossible> {
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
        get_post_ids().bind(|ids| ids.into_iter().map(get_post_info).sequence())
    }

    fn random_crash_page() -> Fetch<String, Exception> {
        Fetch::new(FnRequest::new(move || {
            if !rand::random::<bool>() {
                Err(Exception::Msg("Intended error :P".to_string()))
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
                .map(get_post_content)
                .sequence()
                .bind(|content| {
                    let rendered = render_posts(posts2.into_iter().zip(content.into_iter()));
                    Fetch::<String>::pure(rendered)
                })
        })
    }

    fn error_page(e: Exception) -> Fetch<String> {
        match e {
            Exception::HttpError(err_code) => Fetch::pure(format!("<h1> HttpError: {}", err_code)),
            Exception::Msg(msg) => Fetch::pure(format!(
                "An error occured ... but you received this message: {}",
                msg
            )),
            Exception::OutOfMemory => Fetch::pure("Ooof! Out Of Memory!".into()),
            Exception::Timeout => Fetch::pure("TvT Timeout!".into()),
        }
    }

    fn blog() -> Fetch<String> {
        ap(ap(Fetch::pure(render_page), left_pane()), main_pane())
    }

    fn blog_with_crash() -> Fetch<String, Exception> {
        ap(
            ap(
                ap(
                    Fetch::pure(|x| |y| |z| format!("{}<br>{}", z, render_page(y)(x))),
                    left_pane().into(),
                ),
                main_pane().into(),
            ),
            random_crash_page(),
        )
    }

    #[test]
    fn run_blog() {
        let start_time = time::Instant::now();
        let blog = blog();
        let blog = blog;
        println!("{}", start_time.elapsed().as_millis());

        let _result = blog.run();
        println!("{}", start_time.elapsed().as_millis());
    }
    #[test]
    fn test_random_crash() {
        let blog_with_crash = catch(blog_with_crash(), error_page);
        match blog_with_crash.run() {
            Ok(result) => println!("{}", result),
            Err(e) => match e {},
        }
    }
}
