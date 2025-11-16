//! A combinator for calling a async loader for module before running it's entrypoint.
//! It can be implemeted as `async fn` but this design emit duplicated state machine, and panic handling logic.
//!
//! Panic handling in async fn is bound to `span` and therefore small change of file (like adding a new import)
//!  will change many spans and cause marking of many modules as changed.
//!
//! This structure embed all Location::caller in one place, and therefore simplify incremental splitting.
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

pub struct UnsafeFn<const ASYNC: bool, V, Res> {
    fn_ptr: unsafe extern "C" fn(V) -> Res,
}
pub fn unsafe_fn<const ASYNC: bool, V, Res>(
    fn_ptr: unsafe extern "C" fn(V) -> Res,
) -> UnsafeFn<ASYNC, V, Res> {
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
impl<V, Loader, Res> WamexLoadRunner<V, Loader, UnsafeFn<true, V, Res>, Res>
where
    Res: core::future::Future,
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
impl<V, Loader, Fut> core::future::Future
    for WamexLoadRunner<V, Loader, UnsafeFn<true, V, Fut>, Fut>
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

impl<V, Loader, Res> WamexLoadRunner<V, Loader, UnsafeFn<false, V, Res>, Res> {
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
impl<V, Loader, Res> core::future::Future
    for WamexLoadRunner<V, Loader, UnsafeFn<false, V, Res>, Res>
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
