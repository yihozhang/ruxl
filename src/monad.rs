#[macro_export]
macro_rules! fetch {
  (app {$a:ident <- $ma:expr; $b:ident <- $mb:expr; $c:ident <- $mc:expr; $d:ident <- $md:expr }; $($cont:tt)*) => {
    lift4(move |$a, $b, $c, $d| fetch!($($cont)*), $ma, $mb, $mc, $md).bind(|a| a)
  };

  // return
  (return $r:expr $(;)?) => {
    $crate::Fetch::pure($r)
  };

  // let-binding
  (let $p:pat = $e:expr ; $($r:tt)*) => {{
    let $p = $e;
    fetch!($($r)*)
  }};

  // let-mut-binding
  (let mut $p:pat = $e:expr ; $($r:tt)*) => {{
    let mut $p = $e;
    fetch!($($r)*)
  }};

  // const-bind
  (_ <- $x:expr ; $($r:tt)*) => {
    $x.bind(move |_| { fetch!($($r)*) })
  };

  // bind
  (($binding:pat) <- $x:expr ; $($r:tt)*) => {
    $x.bind(move |$binding| { fetch!($($r)*) })
  };

  ($binding:ident <- $x:expr ; $($r:tt)*) => {
    $x.bind(move |$binding| { fetch!($($r)*) })
  };

  // const-bind
  ($e:expr ; $($a:tt)*) => {
    $e.bind(move |_| fetch!($($a)*))
  };

  // pure
  ($a:expr ) => {
    $a
  };
}
