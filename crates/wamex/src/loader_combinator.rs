//!
//! A combinator for calling a async loader for module before running it's entrypoint.
//! It is extracted from macro generated code to reduce size and reduce emiting of `Location::caller` in loader fn.
//!

use core::future::Future;

pub fn load_and_execute<Args, ResFut>(
    loader: impl Future<Output = bool>,
    constructor: unsafe extern "C" fn(Args) -> ResFut,
    args: Args,
) -> impl Future<Output = ResFut::Output>
where
    ResFut: Future,
{
    async move { load_and_execute_sync(loader, constructor, args).await.await }
}

pub fn load_and_execute_sync<Args, Res>(
    loader: impl Future<Output = bool>,
    constructor: unsafe extern "C" fn(Args) -> Res,
    args: Args,
) -> impl Future<Output = Res> {
    async move {
        let _ = loader.await;
        unsafe { constructor(args) }
    }
}
