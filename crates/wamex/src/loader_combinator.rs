//!
//! A combinator for calling a async loader for module before running it's entrypoint.
//! It is extracted from macro generated code to reduce size and reduce emiting of `Location::caller` in loader fn.
//!
//! Initially it was `async fn` but rustc emit `async fn` with code that handle panic after resume, that contain `Location::caller`.
//! Trying to replace it with `fn (..) -> impl Future` doesn't work well, since `impl Future` need explicit `use<'lifetime>` if any lifetime is present in arguments.
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

// We could replace `NonAsync` marker with `Ready<T>` and remove this code, but only with specialization.
// Since we need to diferentiate sync and async logic based on type signature.
mod _async_or_not {
    use super::*;
    #[doc(hidden)]
    pub trait AsyncOrNot<Args, Loader, Res> {
        type LoaderRunner;
        type SyncResult;

        #[allow(private_interfaces)]
        fn transit_or_finish(
            loader: core::pin::Pin<&mut Self::LoaderRunner>,
        ) -> ResolvedOrCall<Self::SyncResult, Self>
        where
            Self: Sized;
        fn poll_or_unreachable(
            self: core::pin::Pin<&mut Self>,
            _cx: &mut core::task::Context<'_>,
        ) -> core::task::Poll<Self::SyncResult>;
    }

    impl<Args, Loader, Res> AsyncOrNot<Args, Loader, Res> for NonAsync {
        type LoaderRunner = WamexLoadRunner<Args, Loader, UnsafeFn<Args, Res>, NonAsync>;

        type SyncResult = Res;

        #[allow(private_interfaces)]
        fn transit_or_finish(
            mut loader: core::pin::Pin<&mut Self::LoaderRunner>,
        ) -> ResolvedOrCall<Res, NonAsync> {
            match loader
                .as_mut()
                .project_replace(Self::LoaderRunner::Completed)
            {
                WamexLoadRunnerProjReplace::Load {
                    constructor, args, ..
                } => {
                    let res = unsafe { (constructor.fn_ptr)(args) };
                    ResolvedOrCall::Resolved(res)
                }
                _ => unreachable!(),
            }
        }
        fn poll_or_unreachable(
            self: core::pin::Pin<&mut Self>,
            _cx: &mut core::task::Context<'_>,
        ) -> core::task::Poll<Self::SyncResult> {
            unreachable!()
        }
    }
    impl<Args, Loader, Fut> AsyncOrNot<Args, Loader, Fut> for Fut
    where
        Fut: core::future::Future,
    {
        type LoaderRunner = WamexLoadRunner<Args, Loader, UnsafeFn<Args, Fut>, Fut>;
        type SyncResult = Fut::Output;

        #[allow(private_interfaces)]
        fn transit_or_finish(
            mut loader: core::pin::Pin<&mut Self::LoaderRunner>,
        ) -> ResolvedOrCall<Self::SyncResult, Fut> {
            match loader
                .as_mut()
                .project_replace(Self::LoaderRunner::Completed)
            {
                WamexLoadRunnerProjReplace::Load {
                    constructor, args, ..
                } => {
                    let fut = unsafe { (constructor.fn_ptr)(args) };
                    ResolvedOrCall::Call(fut)
                }
                _ => unreachable!(),
            }
        }
        fn poll_or_unreachable(
            mut self: core::pin::Pin<&mut Self>,
            cx: &mut core::task::Context<'_>,
        ) -> core::task::Poll<Self::SyncResult> {
            let res = ready!(self.as_mut().poll(cx));
            core::task::Poll::Ready(res)
        }
    }
    pub(super) enum ResolvedOrCall<Result, Call> {
        Resolved(Result),
        Call(Call),
    }
}

impl<Args, Loader, Res, Any> core::future::Future
    for WamexLoadRunner<Args, Loader, UnsafeFn<Args, Res>, Any>
where
    Any: _async_or_not::AsyncOrNot<Args, Loader, Res, LoaderRunner = Self>,
    Loader: core::future::Future<Output = bool>,
{
    type Output = Any::SyncResult;
    fn poll(
        mut self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        loop {
            let this = self.as_mut().project();
            match this {
                WamexLoadRunnerProj::Load { loader, .. } => {
                    let _res = ready!(loader.poll(cx));
                    match Any::transit_or_finish(self.as_mut()) {
                        _async_or_not::ResolvedOrCall::Resolved(res) => {
                            return core::task::Poll::Ready(res);
                        }
                        _async_or_not::ResolvedOrCall::Call(fut) => {
                            self.as_mut()
                                .project_replace(WamexLoadRunner::Call { caller: fut });
                        }
                    }
                }
                WamexLoadRunnerProj::Call { caller } => {
                    let res = ready!(caller.poll_or_unreachable(cx));
                    return core::task::Poll::Ready(res);
                }
                WamexLoadRunnerProj::Completed => {
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
