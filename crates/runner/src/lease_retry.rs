//! Test/experiment lease renewal decisions, after the HTTP adapter classifies a reply.
//!
//! The caller must preserve source response parsing, cancellation and timestamp
//! conversion order. This state is not the stock evaluator's heartbeat policy.

/// Source renewal attempt result. `Renewed` is constructed only after decoding
/// `lease_expires_at` and converting it onto the process monotonic clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reply {
    Renewed {
        expires_at: f64,
    },
    StaleLease,
    /// An HTTP transport failure or any response other than success/stale lease.
    Transient,
}

/// The caller logs the matching source diagnostic before sleeping or stopping.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decision {
    Renewed { expires_at: f64 },
    Lost,
    Expired,
    Sleep { seconds: f64 },
}

/// One instance per `_renew` call: backoff resets after every successful heartbeat.
pub struct Renewal {
    interval: f64,
    delay: f64,
    lease_expires: f64,
}

impl Renewal {
    #[must_use]
    pub fn new(interval: f64, lease_expires: f64) -> Self {
        Self {
            interval,
            delay: 0.5,
            lease_expires,
        }
    }

    /// Consult the monotonic clock only after a transient failure. A successful
    /// reply has already invoked timestamp conversion; stale lease never does.
    #[must_use]
    pub fn response(&mut self, reply: Reply, clock: impl FnOnce() -> f64) -> Decision {
        match reply {
            Reply::Renewed { expires_at } => Decision::Renewed { expires_at },
            Reply::StaleLease => Decision::Lost,
            Reply::Transient => {
                if self.lease_expires - clock() <= self.delay {
                    Decision::Expired
                } else {
                    Decision::Sleep {
                        seconds: self.delay,
                    }
                }
            }
        }
    }

    /// Call only after the requested sleep completes successfully. Cancellation
    /// or another sleep failure propagates without scheduling another request.
    pub fn slept(&mut self) {
        // Python max/min retain their first operand on unordered comparisons.
        // f64::max/min instead discard NaNs and would change source backoff.
        let interval_cap = if 0.5 > self.interval {
            0.5
        } else {
            self.interval
        };
        let doubled = self.delay * 2.0;
        let bounded = if 30.0 < doubled { 30.0 } else { doubled };
        self.delay = if interval_cap < bounded {
            interval_cap
        } else {
            bounded
        };
    }
}
