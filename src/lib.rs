use std::sync::Arc;
use std::sync::Mutex;
use std::*;

pub trait Request<T> {
    fn run(self) -> T;
}

struct AbsRequest(Box<dyn FnOnce()>);

impl AbsRequest {
    fn run_all(reqs: Vec<AbsRequest>) {
        todo!()
    }
}

enum FetchStatus<T> {
    NotFetched,
    FetchSuccess(T),
}

enum ReqResult<T> {
    Done(T),
    Blocked(Vec<AbsRequest>, Fetch<T>),
}

pub struct Fetch<T>(Box<dyn FnOnce() -> ReqResult<T>>);

impl<T: 'static> From<ReqResult<T>> for Fetch<T> {
    fn from(req_res: ReqResult<T>) -> Self {
        Fetch(Box::new(|| req_res))
    }
}

impl<T: 'static> Fetch<T> {
    pub fn new<R: Request<T> + 'static>(request: R) -> Fetch<T> {
        Fetch(Box::new(|| {
            // TODO: Arc and Mutex seems unnecessary, because
            // there will only ever be two reference, and one
            // is write, one is read
            let status = Arc::new(Mutex::new(FetchStatus::<T>::NotFetched));
            let modifier = status.clone();
            let abs_request = move || {
                let res = request.run();
                let mut m = modifier.as_ref().lock().unwrap();
                *m = FetchStatus::FetchSuccess(res);
            };
            ReqResult::Blocked(vec![AbsRequest(Box::new(abs_request))], Fetch(Box::new(move || {
                let v: &mut FetchStatus<T> = &mut status.as_ref().lock().unwrap();
                if let FetchStatus::FetchSuccess(v) = mem::replace(v, FetchStatus::NotFetched) {
                    ReqResult::Done(v)
                } else {
                    unreachable!()
                }
            })))
        }))
    }
}

impl<T: 'static> Fetch<T> {
    pub fn pure(a: T) -> Fetch<T> {
        Fetch(Box::new(|| ReqResult::Done(a)))
    }

    pub fn pure_fn(f: impl FnOnce() -> T + 'static) -> Fetch<T> {
        Fetch(Box::new(|| ReqResult::Done(f())))
    }

    fn get(self) -> ReqResult<T> {
        (self.0)()
    }

    // TODO: make type Fetch<U, 'a> so U does not to be static
    pub fn bind<U: 'static>(self, k: Box<dyn Fn(T) -> Fetch<U>>) -> Fetch<U> {
        // let res: &ReqResult<T> = &a.0.lock().expect("bind");
        let res: ReqResult<T> = self.get();
        match res {
            ReqResult::Done(a) => k(a),
            ReqResult::Blocked(br, c) => ReqResult::Blocked(br, c.bind(k)).into(),
        }
    }

    pub fn fmap<U: 'static>(self, f: Box<dyn Fn(T) -> U>) -> Fetch<U> {
        let res: ReqResult<T> = self.get();
        match res {
            ReqResult::Done(a) => Fetch::pure(f(a)),
            ReqResult::Blocked(br, c) => ReqResult::Blocked(br, c.fmap(f)).into(),
        }
    }

    pub fn run(self) -> T {
        let r = self.get();
        match r {
            ReqResult::Done(a) => a,
            ReqResult::Blocked(br, c) => {
                AbsRequest::run_all(br);
                c.run()
            }
        }
    }
}

impl<T: 'static, U: 'static> Fetch<Box<dyn Fn(T) -> U>> {
    pub fn ap(self, x: Fetch<T>) -> Fetch<U> {
        match (self.get(), x.get()) {
            (ReqResult::Done(f), ReqResult::Done(x)) => ReqResult::Done(f(x)).into(),
            (ReqResult::Done(f), ReqResult::Blocked(br, c)) => {
                ReqResult::Blocked(br, c.fmap(f)).into()
            }
            (ReqResult::Blocked(br, c), ReqResult::Done(x)) => {
                ReqResult::Blocked(br, c.ap(Fetch::pure(x))).into()
            }
            (ReqResult::Blocked(br1, f), ReqResult::Blocked(br2, x)) => {
                ReqResult::Blocked(vec_merge(br1, br2), f.ap(x)).into()
            }
        }
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
mod tests {}
