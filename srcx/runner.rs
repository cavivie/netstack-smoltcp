use std::{
    future::{Future, IntoFuture},
    pin::Pin,
    task::{Context, Poll},
};

/// BoxFuture acts the same as the [BoxFuture in crate futures utils],
/// which is an owned dynamically typed Future for use in cases where you
/// can’t statically type your result or need to add some indirection.
/// But the difference of this structure is that it will conditionally
/// implement Send according to the properties of type T, which does not
/// require two sets of API interfaces in single-threaded and multi-threaded.
///
/// [BoxFuture in crate futures utils]: https://docs.rs/futures-util/latest/futures_util/future/type.BoxFuture.html
pub struct BoxFuture<'a, T>(Pin<Box<dyn Future<Output = T> + 'a>>);

impl<'a, T> BoxFuture<'a, T> {
    pub fn new<F>(f: F) -> BoxFuture<'a, T>
    where
        F: IntoFuture<Output = T> + 'a,
    {
        BoxFuture(Box::pin(f.into_future()))
    }

    #[allow(unused)]
    pub fn wrap(f: Pin<Box<dyn Future<Output = T> + 'a>>) -> BoxFuture<'a, T> {
        BoxFuture(f)
    }
}

unsafe impl<T: Send> Send for BoxFuture<'_, T> {}

impl<T> Future for BoxFuture<'_, T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.as_mut().poll(context)
    }
}

pub type Runner = BoxFuture<'static, ()>;
