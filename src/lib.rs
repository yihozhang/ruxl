use std::hash::Hash;
use std::sync::Arc;
use std::sync::Mutex;
use std::*;

mod monad;

pub trait Request<T, E = Impossible>: Hash + Clone + Eq {
    fn run(self) -> Result<T, E>;
}

pub enum Impossible {}

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

impl<T: 'static, E: 'static> Into<Fetch<T, E>> for Result<T, E> {
    fn into(self) -> Fetch<T, E> {
        match self {
            Ok(res) => Fetch::pure(res),
            Err(e) => throw(e),
        }
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
macro_rules! lift_builder {
    ($func:ident; $F:ident; $U:ident; $($T:ident),+; $f:ident; $($x:ident),+) => {
        pub fn $func<
            E:'static,
            $($T:'static),+,
            U:'static,
            F:FnOnce($($T),+) -> $U + 'static
        >($f: $F, $($x:crate::Fetch<$T,E>),+) -> crate::Fetch<$U,E> {
            let $f = crate::Fetch::pure($f);
            lift_builder!(@fmap $f, $($x),+);
            lift_builder!(@ap $f; $($x),+)
        }
    };
    (@fmap $f:ident, $($x:ident),+) => {
        let $f = $f.fmap(|$f| lift_builder!(@fmap_lambda $($x),+; $f, $($x),+));
    };
    (@fmap_lambda $x:ident; $f:ident, $($args:ident),+) => {
        |$x| $f($($args),+)
    };
    (@fmap_lambda $x:ident, $($xs:ident),*; $($args:ident),+) => {
        |$x| lift_builder!(@fmap_lambda $($xs),*; $($args),+)
    };
    (@ap $f:expr; $x:ident, $($xs:ident),*) => {
        lift_builder!(@ap crate::ap($f, $x); $($xs),*)
    };
    (@ap $f:expr; $x:ident) => {
        crate::ap($f, $x)
    };
}

lift_builder!(lift2; F; U; T1, T2; f; x1, x2);
lift_builder!(lift3; F; U; T1, T2, T3; f; x1, x2, x3);
lift_builder!(lift4; F; U; T1, T2, T3, T4; f; x1, x2, x3, x4);
lift_builder!(lift5; F; U; T1, T2, T3, T4, T5; f; x1, x2, x3, x4, x5);

fn cons_f<T: 'static, E: 'static>(ys: Fetch<Vec<T>, E>, x: Fetch<T, E>) -> Fetch<Vec<T>, E> {
    lift2(
        |mut ys: Vec<T>, x| {
            ys.push(x);
            ys
        },
        ys,
        x,
    )
}

pub trait Traversable<T> {
    fn traverse<T2: 'static, E: 'static>(
        self,
        f: impl Fn(T) -> Fetch<T2, E> + 'static,
    ) -> Fetch<Vec<T2>, E>;
}

impl<T, I: Iterator<Item = T>> Traversable<T> for I {
    fn traverse<T2: 'static, E: 'static>(
        self,
        f: impl Fn(T) -> Fetch<T2, E> + 'static,
    ) -> Fetch<Vec<T2>, E> {
        let init = Fetch::pure(Vec::new());
        self.fold(init, |ys, x| cons_f(ys, f(x)))
    }
}

pub trait Sequence<T, E> {
    fn sequence(self) -> Fetch<Vec<T>, E>;
}

impl<T: 'static, E: 'static, V: Iterator<Item = Fetch<T, E>> + 'static> Sequence<T, E> for V {
    fn sequence(self) -> Fetch<Vec<T>, E> {
        // let init = Fetch::pure(Vec::new());
        // self.fold(init, cons_f)
        self.traverse(|x| x)
    }
}

fn vec_merge<T>(mut a: Vec<T>, mut b: Vec<T>) -> Vec<T> {
    if a.len() < b.len() {
        mem::swap(&mut a, &mut b);
    }
    a.append(&mut b);
    a
}

pub fn while_m<T: 'static>(mut f: impl FnMut() -> Fetch<bool, T> + 'static) -> Fetch<(), T> {
    fetch! {
        let fetch_b = f();
        b <- fetch_b;
        if b {
            while_m(f)
        } else {
            Fetch::pure(())
        }
    }
}

#[cfg(test)]
mod tests {
    #[derive(Hash, PartialEq, Eq, Clone, Debug)]
    pub enum Exception {
        Msg(String),
    }

    use std::time::Duration;

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

    #[derive(Clone)]
    struct SleepRequest<T> {
        name: &'static str,
        sleep_duration: u64,
        result: T,
    }

    // we use name for eq and hash here, only for demonstrating purpose
    // a real request will not have result stored in it.
    // Then we will be able to derive Hash and Eq directly

    impl<T> Hash for SleepRequest<T> {
        fn hash<H: hash::Hasher>(&self, state: &mut H) {
            self.name.hash(state)
        }
    }

    impl<T> PartialEq for SleepRequest<T> {
        fn eq(&self, other: &Self) -> bool {
            self.name.eq(other.name)
        }
    }

    impl<T> Eq for SleepRequest<T> {}

    impl<T: Clone> Request<T> for SleepRequest<T> {
        fn run(self) -> Result<T, Impossible> {
            thread::sleep(Duration::from_millis(self.sleep_duration));
            Ok(self.result)
        }
    }

    fn get_post_ids() -> Fetch<Vec<PostId>> {
        Fetch::new(SleepRequest {
            name: "get_post_ids",
            sleep_duration: 500,
            result: vec![PostId(1), PostId(2)],
        })
    }

    fn get_post_info(id: PostId) -> Fetch<PostInfo> {
        Fetch::new(SleepRequest {
            name: "get_post_info",
            sleep_duration: 500,
            result: PostInfo {
                id,
                date: Date("today".to_string()),
                topic: ["Hello", "world"][id.0 % 2].to_string(),
            },
        })
    }

    fn get_post_content(id: PostId) -> Fetch<PostContent, Impossible> {
        Fetch::new(SleepRequest {
            name: "get_post_content",
            sleep_duration: 500,
            result: PostContent(format!("A post with id {}", id.0)),
        })
    }

    fn render_posts(it: impl Iterator<Item = (PostInfo, PostContent)>) -> String {
        it.map(|(info, content)| format!("<p>{} {}</p>", info.topic, content.0))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_side_pane(posts: String, topics: String) -> String {
        format!(
            "<div class=\"topics\">{}</div>\n<div class=\"posts\">{}</div>",
            topics, posts
        )
    }

    fn render_page(side: String, main: String) -> String {
        format!(
            "<html><body><div>{}</div><div>{}</div></body></html>",
            side, main
        )
    }

    fn popular_posts() -> Fetch<String> {
        Fetch::new(SleepRequest {
            name: "popular_posts",
            sleep_duration: 500,
            result: "<p>popular post 1, popular post 2, ...</p>\n".to_string(),
        })
    }

    fn topics() -> Fetch<String> {
        Fetch::new(SleepRequest {
            name: "topics",
            sleep_duration: 500,
            result: "<p>topic 1, topic 2, ...</p>\n".to_string(),
        })
    }

    fn left_pane() -> Fetch<String> {
        lift2(render_side_pane, popular_posts(), topics())
    }

    fn get_all_post_info() -> Fetch<Vec<PostInfo>> {
        fetch! {
            ids <- get_post_ids();
            ids.into_iter().traverse(get_post_info)
        }
    }

    #[derive(Hash, Clone, PartialEq, Eq)]
    struct RandomCrashRequest<T, E> {
        err: E,
        result: T,
    }

    impl<T, E> Request<T, E> for RandomCrashRequest<T, E>
    where
        T: Hash + Eq + Clone,
        E: Hash + Eq + Clone,
    {
        fn run(self) -> Result<T, E> {
            if !rand::random::<bool>() {
                Err(self.err)
            } else {
                Ok(self.result)
            }
        }
    }

    fn random_crash_page() -> Fetch<String, Exception> {
        Fetch::new(RandomCrashRequest {
            err: Exception::Msg("Intended error :P".to_string()),
            result: "".to_string(),
        })
    }

    fn main_pane() -> Fetch<String> {
        // get_all_post_info().bind(|posts| {
        //     let posts2 = posts.clone();
        //     posts
        //         .into_iter()
        //         .map(|post| post.id)
        //         .map(get_post_content)
        //         .sequence()
        //         .bind(|content| {
        //             let rendered = render_posts(posts2.into_iter().zip(content.into_iter()));
        //             Fetch::<String>::pure(rendered)
        //         })
        // })
        fetch! {
            posts <- get_all_post_info();
            let posts2 = posts.clone();
            content <- posts.into_iter().traverse(|post| get_post_content(post.id));
            let rendered = render_posts(posts2.into_iter().zip(content.into_iter()));
            return rendered
        }
    }

    fn error_page(e: Exception) -> Fetch<String> {
        match e {
            // Exception::HttpError(err_code) => Fetch::pure(format!("<h1> HttpError: {}", err_code)),
            Exception::Msg(msg) => Fetch::pure(format!(
                "An error occured ... but you received this message: {}",
                msg
            )),
            // Exception::OutOfMemory => Fetch::pure("Ooof! Out Of Memory!".into()),
            // Exception::Timeout => Fetch::pure("TvT Timeout!".into()),
        }
    }

    fn blog() -> Fetch<String> {
        lift2(render_page, left_pane(), main_pane())
    }

    fn blog_with_crash() -> Fetch<String, Exception> {
        lift3(
            |x, y, z| format!("{}<br>{}", z, render_page(y, x)),
            left_pane().into(),
            main_pane().into(),
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
