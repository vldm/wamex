//!
//! A combinator for calling a async loader for module before running it's entrypoint.
//! It is extracted from macro generated code to reduce size and reduce emiting of `Location::caller` in loader fn.
//!
//! Initially it was `async fn` but rustc emit `async fn` with panic after resume, that contain `Location::caller`.
//! Trying to replace it with `fn (..) -> impl Future` doesn't work, since `impl Future` need explicit `use<'lifetime>` if any lifetime is present in arguments.
//!

use core::future::Future;

pin_project_lite::pin_project! {
    #[project = WamexLoadRunnerProj]
    #[project_replace = WamexLoadRunnerProjReplace]
    pub enum WamexLoadRunner<V, Loader, Constructor, Res>
    {
        Load {
            #[pin]
            loader: Loader,
            constructor: Constructor,
            args: V,
        },
        Call {
            #[pin]
            caller: Res,
        },
        Completed,
    }
}

impl<V, Loader, Constructor, Res> WamexLoadRunner<V, Loader, Constructor, Res> {
    pub fn new(loader: Loader, constructor: Constructor, args: V) -> Self {
        WamexLoadRunner::Load {
            args,
            loader,
            constructor,
        }
    }
}

pub struct UnsafeFn<V, Res> {
    fn_ptr: unsafe extern "C" fn(V) -> Res,
}
pub fn unsafe_fn<V, Res>(fn_ptr: unsafe extern "C" fn(V) -> Res) -> UnsafeFn<V, Res> {
    UnsafeFn { fn_ptr }
}

macro_rules! ready {
    ($e:expr $(,)?) => {
        match $e {
            core::task::Poll::Ready(t) => t,
            core::task::Poll::Pending => return core::task::Poll::Pending,
        }
    };
}

// async impl
impl<V, Loader, Fut> WamexLoadRunner<V, Loader, UnsafeFn<V, Fut>, Fut>
where
    Fut: core::future::Future,
{
    fn transit_to_call(mut self: core::pin::Pin<&mut Self>) {
        match self.as_mut().project_replace(Self::Completed) {
            WamexLoadRunnerProjReplace::Load {
                constructor, args, ..
            } => {
                let fut = unsafe { (constructor.fn_ptr)(args) };
                self.project_replace(WamexLoadRunner::Call { caller: fut });
            }
            _ => unreachable!(),
        }
    }
}
impl<V, Loader, Fut> core::future::Future for WamexLoadRunner<V, Loader, UnsafeFn<V, Fut>, Fut>
where
    Fut: core::future::Future,
    Loader: core::future::Future<Output = bool>,
{
    type Output = Fut::Output;
    fn poll(
        mut self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        loop {
            let this = self.as_mut().project();
            match this {
                WamexLoadRunnerProj::Load { loader, .. } => {
                    let _res = ready!(loader.poll(cx));
                    self.as_mut().transit_to_call();
                }
                WamexLoadRunnerProj::Call { caller } => {
                    let res = ready!(caller.poll(cx));
                    return core::task::Poll::Ready(res);
                }
                WamexLoadRunnerProj::Completed => {
                    panic!("polled after completion");
                }
            }
        }
    }
}

// Sync impl
impl<V, Loader, Res> WamexLoadRunner<V, Loader, UnsafeFn<V, Res>, NonAsync> {
    fn completed(mut self: core::pin::Pin<&mut Self>) -> Res {
        match self.as_mut().project_replace(Self::Completed) {
            WamexLoadRunnerProjReplace::Load {
                constructor, args, ..
            } => {
                let fut = unsafe { (constructor.fn_ptr)(args) };
                fut
            }
            _ => unreachable!(),
        }
    }
}

impl<V, Loader, Res> core::future::Future for WamexLoadRunner<V, Loader, UnsafeFn<V, Res>, NonAsync>
where
    Loader: core::future::Future<Output = bool>,
{
    type Output = Res;
    fn poll(
        mut self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        loop {
            let this = self.as_mut().project();
            match this {
                WamexLoadRunnerProj::Load { loader, .. } => {
                    let _res = ready!(loader.poll(cx));
                    return core::task::Poll::Ready(self.as_mut().completed());
                }
                WamexLoadRunnerProj::Call { .. } | WamexLoadRunnerProj::Completed => {
                    panic!("polled after completion");
                }
            }
        }
    }
}

pub enum NonAsync {}

#[inline(never)]
pub fn load_and_execute<Args, ResFut, Res>(
    loader: impl Future<Output = bool>,
    constructor: unsafe extern "C" fn(Args) -> ResFut,
    args: Args,
) -> WamexLoadRunner<Args, impl Future<Output = bool>, UnsafeFn<Args, ResFut>, ResFut>
where
    ResFut: Future<Output = Res>,
{
    WamexLoadRunner::<Args, _, UnsafeFn<Args, ResFut>, ResFut>::new(
        loader,
        unsafe_fn::<Args, ResFut>(constructor),
        args,
    )
}

#[inline(never)]
pub fn load_and_execute_sync<Args, Loader, Res>(
    loader: Loader,
    constructor: unsafe extern "C" fn(Args) -> Res,
    args: Args,
) -> WamexLoadRunner<Args, Loader, UnsafeFn<Args, Res>, NonAsync> {
    WamexLoadRunner::<Args, Loader, UnsafeFn<Args, Res>, NonAsync>::new(
        loader,
        unsafe_fn::<Args, Res>(constructor),
        args,
    )
}
