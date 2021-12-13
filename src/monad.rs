#[macro_export]
macro_rules! fetch {
  // return
  (return $r:expr $(;)?) => {
    $crate::Fetch::pure($r)
  };

  // let-binding
  (let $p:pat = $e:expr ; $($r:tt)*) => {{
    let $p = $e;
    fetch!($($r)*)
  }};

  // const-bind
  (_ <- $x:expr ; $($r:tt)*) => {
    $x.bind(move |_| { fetch!($($r)*) })
  };

  // bind
  ($binding:ident <- $x:expr ; $($r:tt)*) => {
    $x.bind(move |$binding| { fetch!($($r)*) })
  };

  // const-bind
  ($e:expr ; $($a:tt)*) => {
    $e.bind(move |_| m!($($a)*))
  };

  // pure
  ($a:expr) => {
    $a
  }
}
