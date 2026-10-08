use crate::*;
use std::{
    future::Future,
    marker::PhantomPinned,
    mem::{MaybeUninit, align_of, size_of},
    pin::Pin,
    task::{Context, Poll},
};

use crate::{HttpResponse, IntoResponse};

pub(crate) type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

pub(crate) const INLINE_FUTURE_SIZE: usize = 64;

#[repr(align(16))]
pub(crate) struct FutureStorage([MaybeUninit<u8>; INLINE_FUTURE_SIZE]);

pub(crate) struct InlineFuture {
    storage: FutureStorage,
    poll_fn: unsafe fn(*mut u8, &mut Context<'_>) -> Poll<HttpResponse>,
    drop_fn: unsafe fn(*mut u8),
    _pin: PhantomPinned,
}

impl InlineFuture {
    pub(crate) fn new<F, R>(future: F) -> Self
    where
        F: Future<Output = R> + Send + 'static,
        R: IntoResponse,
    {
        // These are the invariants every `unsafe` block below relies on. The
        // comparisons are on compile-time constants, so the checks fold away.
        assert!(size_of::<F>() <= INLINE_FUTURE_SIZE);
        assert!(align_of::<F>() <= align_of::<FutureStorage>());
        let mut storage = FutureStorage([MaybeUninit::uninit(); INLINE_FUTURE_SIZE]);
        // SAFETY: the asserts above guarantee `F` fits in, and is aligned for,
        // `storage`, which is uninitialized and exclusively owned here.
        unsafe {
            (storage.0.as_mut_ptr() as *mut F).write(future);
        }
        Self {
            storage,
            poll_fn: poll_response::<F, R>,
            drop_fn: drop_inline::<F>,
            _pin: PhantomPinned,
        }
    }
}

impl Drop for InlineFuture {
    fn drop(&mut self) {
        // SAFETY: `drop_fn` is `drop_inline::<F>` for the `F` written by `new`,
        // and `storage` holds that `F` until this single drop.
        unsafe { (self.drop_fn)(self.storage.0.as_mut_ptr() as *mut u8) };
    }
}

unsafe fn poll_response<F, R>(storage: *mut u8, context: &mut Context<'_>) -> Poll<HttpResponse>
where
    F: Future<Output = R> + Send + 'static,
    R: IntoResponse,
{
    // SAFETY: `storage` holds an initialized `F` that is never moved after the
    // owning `InlineFuture` is pinned (`PhantomPinned`), so pinning it is sound.
    match unsafe { Pin::new_unchecked(&mut *(storage as *mut F)).poll(context) } {
        Poll::Ready(value) => Poll::Ready(value.into_response()),
        Poll::Pending => Poll::Pending,
    }
}

unsafe fn drop_inline<F>(storage: *mut u8)
where
    F: Send + 'static,
{
    // SAFETY: called exactly once, from `Drop`, on storage holding a valid `F`.
    unsafe { std::ptr::drop_in_place(storage as *mut F) };
}

#[doc(hidden)]
pub struct HandlerFuture(pub(crate) HandlerFutureKind);

pub(crate) enum HandlerFutureKind {
    Inline(InlineFuture),
    Boxed(BoxFuture<HttpResponse>),
}

impl HandlerFuture {
    pub(crate) fn from_response_future<F, R>(future: F) -> Self
    where
        F: Future<Output = R> + Send + 'static,
        R: IntoResponse,
    {
        if size_of::<F>() <= INLINE_FUTURE_SIZE && align_of::<F>() <= align_of::<FutureStorage>() {
            Self(HandlerFutureKind::Inline(InlineFuture::new::<F, R>(future)))
        } else {
            Self(HandlerFutureKind::Boxed(Box::pin(async move {
                future.await.into_response()
            })))
        }
    }
}

impl Future for HandlerFuture {
    type Output = HttpResponse;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: neither variant is moved out of `self`; the inline future is
        // polled in place and the boxed future is already heap-pinned.
        unsafe {
            match &mut self.get_unchecked_mut().0 {
                HandlerFutureKind::Inline(future) => {
                    (future.poll_fn)(future.storage.0.as_mut_ptr() as *mut u8, context)
                }
                HandlerFutureKind::Boxed(future) => Pin::new_unchecked(future).poll(context),
            }
        }
    }
}

/// A handler implementation is monomorphized at registration time and stored
/// as a single erased service only at the router boundary.
pub trait Handler<S, Args>: Send + Sync + 'static {
    type Response: ResponseMetadata;
    const NEEDS_PARAMS: bool = false;
    /// Whether any extractor reads path captures from the router.
    const NEEDS_CAPTURE: bool = false;
    const NEEDS_BODY: bool = false;

    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest::default()
    }

    fn call(&self, request: &mut Request<Bytes>, params: &Params, state: &Arc<S>) -> HandlerFuture;

    fn zero_handler(&self) -> Option<ErasedZeroHandler> {
        None
    }
}

/// An escape hatch for handlers that need Hyper's original streaming request
/// body. Unlike typed extractors, a raw handler receives `Incoming` without
/// the framework collecting it first.
pub trait RawHandler<S>: Send + Sync + 'static {
    type Response: ResponseMetadata;

    fn call(&self, request: Request<Incoming>) -> HandlerFuture;
}

impl<S, F, Fut, R> RawHandler<S> for F
where
    S: Send + Sync + 'static,
    F: Fn(Request<Incoming>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: IntoResponse + ResponseMetadata,
{
    type Response = R;

    fn call(&self, request: Request<Incoming>) -> HandlerFuture {
        let future = (self)(request);
        HandlerFuture::from_response_future(future)
    }
}

impl<S, F, Fut, R> Handler<S, ()> for F
where
    S: Send + Sync + 'static,
    F: Fn() -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: IntoResponse + ResponseMetadata,
{
    type Response = R;
    const NEEDS_PARAMS: bool = false;
    const NEEDS_BODY: bool = false;

    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest::default()
    }

    fn call(
        &self,
        _request: &mut Request<Bytes>,
        _params: &Params,
        _state: &Arc<S>,
    ) -> HandlerFuture {
        let future = (self)();
        HandlerFuture::from_response_future(future)
    }

    fn zero_handler(&self) -> Option<ErasedZeroHandler> {
        let handler = self.clone();
        Some(Box::new(move || {
            let future = (handler)();
            HandlerFuture::from_response_future(future)
        }))
    }
}

impl<S, F, Fut, R, E1> Handler<S, (E1,)> for F
where
    S: Send + Sync + 'static,
    F: Fn(E1) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: IntoResponse + ResponseMetadata,
    E1: FromRequest<S>,
{
    type Response = R;
    const NEEDS_PARAMS: bool = E1::NEEDS_PARAMS;
    const NEEDS_CAPTURE: bool = E1::NEEDS_CAPTURE;
    const NEEDS_BODY: bool = E1::NEEDS_BODY;

    fn openapi_request() -> OpenApiRequest {
        E1::openapi_request()
    }

    fn call(&self, request: &mut Request<Bytes>, params: &Params, state: &Arc<S>) -> HandlerFuture {
        let value = match E1::from_request(request, params, state) {
            Ok(value) => value,
            Err(error) => return HandlerFuture::from_response_future(future::ready(error)),
        };
        HandlerFuture::from_response_future((self)(value))
    }
}

macro_rules! impl_extractor_handler {
    (
        $first:ident : $first_arg:ident
        $(, $rest:ident : $rest_arg:ident)+ $(,)?
    ) => {
        impl<S, F, Fut, R, $first $(, $rest)*> Handler<S, ($first $(, $rest)*,)> for F
        where
            S: Send + Sync + 'static,
            F: Fn($first $(, $rest)*) -> Fut + Send + Sync + 'static,
            Fut: Future<Output = R> + Send + 'static,
            R: IntoResponse + ResponseMetadata,
            $first: FromRequest<S>,
            $($rest: FromRequest<S>,)*
        {
            type Response = R;
            const NEEDS_PARAMS: bool =
                <$first as FromRequest<S>>::NEEDS_PARAMS
                $(|| <$rest as FromRequest<S>>::NEEDS_PARAMS)*;
            const NEEDS_CAPTURE: bool =
                <$first as FromRequest<S>>::NEEDS_CAPTURE
                $(|| <$rest as FromRequest<S>>::NEEDS_CAPTURE)*;
            const NEEDS_BODY: bool =
                <$first as FromRequest<S>>::NEEDS_BODY
                $(|| <$rest as FromRequest<S>>::NEEDS_BODY)*;

            fn openapi_request() -> OpenApiRequest {
                let mut metadata = <$first as FromRequest<S>>::openapi_request();
                $(metadata.merge(<$rest as FromRequest<S>>::openapi_request());)*
                metadata
            }

            fn call(
                &self,
                request: &mut Request<Bytes>,
                params: &Params,
                state: &Arc<S>,
            ) -> HandlerFuture {
                let $first_arg = match <$first as FromRequest<S>>::from_request(
                    request, params, state,
                ) {
                    Ok(value) => value,
                    Err(error) => return HandlerFuture::from_response_future(future::ready(error)),
                };
                $(
                    let $rest_arg = match <$rest as FromRequest<S>>::from_request(
                        request, params, state,
                    ) {
                        Ok(value) => value,
                        Err(error) => {
                            return HandlerFuture::from_response_future(future::ready(error));
                        }
                    };
                )*
                let future = (self)($first_arg $(, $rest_arg)*);
                HandlerFuture::from_response_future(future)
            }
        }
    };
}

impl_extractor_handler!(E1: first, E2: second);
impl_extractor_handler!(E1: first, E2: second, E3: third);
impl_extractor_handler!(E1: first, E2: second, E3: third, E4: fourth);
impl_extractor_handler!(E1: first, E2: second, E3: third, E4: fourth, E5: fifth);
impl_extractor_handler!(E1: first, E2: second, E3: third, E4: fourth, E5: fifth, E6: sixth);
impl_extractor_handler!(E1: first, E2: second, E3: third, E4: fourth, E5: fifth, E6: sixth, E7: seventh);
impl_extractor_handler!(E1: first, E2: second, E3: third, E4: fourth, E5: fifth, E6: sixth, E7: seventh, E8: eighth);
