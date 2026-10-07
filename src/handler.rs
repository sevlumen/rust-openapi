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
