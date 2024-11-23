#![allow(dead_code)]

use std::{
    borrow::BorrowMut,
    future::Future,
    sync::{Arc, Mutex},
    task::Poll,
    time::Duration,
};

use futures_core::TryFuture;
use tokio::time::Instant;

pub struct SharedState {
    state: State,
    failure_count: i32,
}

#[derive(Clone)]
pub struct CircuitBreaker {
    shared: Arc<Mutex<SharedState>>,
    failure_threshold: i32,
    reset_timeout: Duration,
}

impl CircuitBreaker {
    fn new(failure_threshold: i32, reset_timeout: Duration) -> Self {
        let failure_count = 0;
        CircuitBreaker {
            shared: Arc::new(Mutex::new(SharedState {
                state: State::new(),
                failure_count,
            })),
            failure_threshold,
            reset_timeout,
        }
    }

    fn call<P, F, R, E>(&mut self, p: P, f: F) -> Result<R, Error<E>>
    where
        F: FnOnce() -> Result<R, E>,
        P: FnOnce(&E) -> bool,
    {
        if !self.allow_call() {
            return Err(Error::Rejected);
        }

        match f() {
            Ok(r) => {
                self.on_sucess();
                Ok(r)
            }
            Err(e) => {
                if p(&e) {
                    self.on_failure()
                } else {
                    self.on_sucess()
                };
                Err(Error::Inner(e))
            }
        }
    }

    fn call_async<P, F>(&mut self, p: P, f: F, timeout: Option<Duration>) -> TripFuture<F, P>
    where
        F: TryFuture,
        P: FnOnce(&F::Error) -> bool + Copy,
    {
        let timeout = timeout.map(|d| Instant::now() + d);
        TripFuture {
            future: f,
            predicate: p,
            breaker: self.clone(),
            allowed: false,
            timeout,
        }
    }

    #[inline]
    pub(crate) fn tripped(&self) -> bool {
        self.shared.lock().unwrap().state.tripped()
    }

    #[inline]
    fn allow_call(&mut self) -> bool {
        let mut shared = self.shared.lock().unwrap();
        match shared.state {
            State::Closed => true,
            State::HalfOpen => true,
            State::Open(_delay, until) => {
                if Instant::now() > until {
                    shared.state = State::HalfOpen;
                    true
                } else {
                    false
                }
            }
        }
    }

    #[inline]
    fn on_sucess(&mut self) {
        let mut shared = self.shared.borrow_mut().lock().unwrap();
        if shared.state == State::HalfOpen {
            shared.state = State::Closed;
            shared.failure_count = 0;
        }
    }

    #[inline]
    fn on_failure(&mut self) {
        let mut shared = self.shared.borrow_mut().lock().unwrap();
        shared.failure_count += 1;
        if shared.failure_count >= self.failure_threshold {
            shared.state = State::Open(self.reset_timeout, Instant::now() + self.reset_timeout);
        }
    }
}

pin_project_lite::pin_project! {
    struct TripFuture<F, P> {
        #[pin]
        future: F,
        predicate: P,
        breaker: CircuitBreaker,
        allowed: bool,
        timeout: Option<Instant>,
    }
}

impl<F, P> Future for TripFuture<F, P>
where
    F: TryFuture,
    P: FnOnce(&F::Error) -> bool + Copy,
{
    type Output = Result<F::Ok, Error<F::Error>>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let pinned = self.project();

        if let Some(t) = pinned.timeout {
            let now = Instant::now();
            if now > *t {
                return Poll::Ready(Err(Error::Timeout));
            }
        }

        if !*pinned.allowed {
            *pinned.allowed = true;
            if !pinned.breaker.allow_call() {
                return Poll::Ready(Err(Error::Rejected));
            }
        }

        match pinned.future.try_poll(cx) {
            Poll::Ready(Ok(r)) => {
                pinned.breaker.on_sucess();
                Poll::Ready(Ok(r))
            }
            Poll::Ready(Err(e)) => {
                if !(pinned.predicate)(&e) {
                    pinned.breaker.on_sucess()
                } else {
                    pinned.breaker.on_failure()
                }
                Poll::Ready(Err(Error::Inner(e)))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum State {
    Closed,
    Open(Duration, Instant),
    HalfOpen,
}

impl State {
    pub fn new() -> Self {
        State::Closed
    }

    #[inline]
    fn tripped(&self) -> bool {
        // returns true when the circuit break has been tripped
        matches!(self, Self::Open(_, _))
    }
}

#[derive(Debug)]
pub enum Error<E> {
    Inner(E),
    Rejected,
    Timeout,
}

impl<E> std::fmt::Display for Error<E>
where
    E: std::fmt::Display,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inner(e) => write!(f, "{}", e),
            Self::Rejected => write!(f, "rejected"),
            Self::Timeout => write!(f, "timeout"),
        }
    }
}

impl<E> PartialEq for Error<E> {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Inner(_), Self::Inner(_))
                | (Self::Rejected, Self::Rejected)
                | (Self::Timeout, Self::Timeout)
        )
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for Error<E> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use tokio::time::sleep as tokio_sleep;

    #[derive(Debug, PartialEq)]
    enum TestError {
        Trip,
        DontTrip,
    }

    impl std::fmt::Display for TestError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:?}", self)
        }
    }

    impl std::error::Error for TestError {}

    const fn is_err(e: &TestError) -> bool {
        match e {
            TestError::Trip => true,
            TestError::DontTrip => false,
        }
    }

    #[test]
    fn test_call_ok() {
        let mut cb = CircuitBreaker::new(3, Duration::from_secs(1));
        assert_eq!(cb.shared.lock().unwrap().state, State::Closed);
        assert!(cb.call(is_err, || Ok(())).is_ok());
        assert!(!cb.tripped())
    }

    #[test]
    fn test_trip() {
        let failure_count = 3;
        let mut cb = CircuitBreaker::new(failure_count, Duration::from_secs(1));
        for _ in 0..failure_count {
            assert!(cb
                .call(is_err, || Err::<(), _>(TestError::Trip))
                .is_err());
        }

        assert!(cb.tripped());
        assert_eq!(
            cb.call(is_err, || Ok(()))
                .expect_err("returned ok when breaker tripped"),
            Error::Rejected
        );
    }

    #[test]
    fn test_ignore_error() {
        let failure_count = 3;
        let reset_timeout = Duration::from_secs(1);
        let mut cb = CircuitBreaker::new(failure_count, reset_timeout);
        for _ in 0..failure_count {
            assert!(cb
                .call(is_err, || Err::<(), _>(TestError::DontTrip))
                .is_err());
        }
        assert!(!cb.tripped());
    }

    #[test]
    fn test_reset_timeout() {
        let failure_count = 1;
        let reset_timeout = Duration::from_millis(100);
        let mut cb = CircuitBreaker::new(failure_count, reset_timeout);

        for _ in 0..failure_count {
            assert!(cb
                .call(is_err, || Err::<(), _>(TestError::Trip))
                .is_err());
        }
        assert!(cb.tripped());

        assert_eq!(
            cb.call(is_err, || Ok(()))
                .expect_err("returned ok when breaker tripped"),
            Error::Rejected
        );

        sleep(reset_timeout);

        assert!(cb.call(is_err, || Ok(())).is_ok());
        assert!(!cb.tripped());
    }

    #[tokio::test]
    async fn test_async_call() {
        let failure_count = 3;
        let reset_timeout = Duration::from_secs(1);
        let mut cb = CircuitBreaker::new(failure_count, reset_timeout);
        let fut = async {
            tokio_sleep(Duration::from_millis(100)).await;
            Ok::<(), TestError>(())
        };
        let res = cb.call_async(is_err, fut, None).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_async_trip() {
        let failure_count = 3;
        let reset_timeout = Duration::from_millis(200);
        let mut cb = CircuitBreaker::new(failure_count, reset_timeout);
        for _ in 0..failure_count {
            let fut = async {
                tokio_sleep(Duration::from_millis(100)).await;
                Err::<(), _>(TestError::Trip)
            };
            let res = cb.call_async(is_err, fut, None).await;
            assert!(res.is_err());
        }
        let fut = async {
            tokio_sleep(Duration::from_millis(100)).await;
            Ok(())
        };
        let res = cb.call_async(is_err, fut, None).await;
        assert!(res.is_err());

        tokio_sleep(reset_timeout).await;

        let fut = async {
            tokio_sleep(Duration::from_millis(100)).await;
            Ok(())
        };
        let res = cb.call_async(is_err, fut, None).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_timeout() {
        let failure_count = 3;
        let reset_timeout = Duration::from_secs(1);
        let mut cb = CircuitBreaker::new(failure_count, reset_timeout);
        let fut = async {
            tokio_sleep(Duration::from_millis(100)).await;
            Ok(())
        };
        let res = cb
            .call_async(is_err, fut, Some(Duration::from_millis(50)))
            .await;
        match res {
            Err(e) => {
                assert_eq!(e, Error::Timeout);
            }
            _ => panic!("expected timeout"),
        }
    }
}
