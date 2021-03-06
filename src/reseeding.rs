// Copyright 2013 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// https://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! A wrapper around another PRNG that reseeds it after it
//! generates a certain number of random bytes.

use {RngCore, SeedableRng, Error, ErrorKind};

/// A wrapper around any PRNG which reseeds the underlying PRNG after it has
/// generated a certain number of random bytes.
///
/// Reseeding is never strictly *necessary*. Cryptographic PRNGs don't have a
/// limited number of bytes they can output, or at least not a limit reachable
/// in any practical way. There is no such thing as 'running out of entropy'.
///
/// Some small non-cryptographic PRNGs can have very small periods, for
/// example less than 2<sup>64</sup>. Would reseeding help to ensure that you do
/// not wrap around at the end of the period? A period of 2<sup>64</sup> still
/// takes several centuries of CPU-years on current hardware. Reseeding will
/// actually make things worse, because the reseeded PRNG will just continue
/// somewhere else *in the same period*, with a high chance of overlapping with
/// previously used parts of it.
///
/// # When should you use `ReseedingRng`?
///
/// - Reseeding can be seen as some form of 'security in depth'. Even if in the
///   future a cryptographic weakness is found in the CSPRNG being used,
///   occasionally reseeding should make exploiting it much more difficult or
///   even impossible.
/// - It can be used as a poor man's cryptography (not recommended, just use a
///   good CSPRNG). Previous implementations of `thread_rng` for example used
///   `ReseedingRng` with the ISAAC RNG. That algorithm, although apparently
///   strong and with no known attack, does not come with any proof of security
///   and does not meet the current standards for a cryptographically secure
///   PRNG. By reseeding it frequently (every 32 MiB) it seems safe to assume
///   there is no attack that can operate on the tiny window between reseeds.
///
/// # Error handling
///
/// If reseeding fails, `try_fill_bytes` is the only `Rng` method to report it.
/// For all other `Rng` methods, `ReseedingRng` will not panic but try to
/// handle the error intelligently; if handling the source error fails these
/// methods will continue generating data from the wrapped PRNG without
/// reseeding.
///
/// It is usually best to use the infallible methods `next_u32`, `next_u64` and
/// `fill_bytes` because they can make use of this error handling strategy.
/// Use `try_fill_bytes` and possibly `try_reseed` if you want to handle
/// reseeding errors explicitly.
#[derive(Debug)]
pub struct ReseedingRng<R, Rsdr> {
    rng: R,
    reseeder: Rsdr,
    threshold: i64,
    bytes_until_reseed: i64,
}

impl<R: RngCore + SeedableRng, Rsdr: RngCore> ReseedingRng<R, Rsdr> {
    /// Create a new `ReseedingRng` with the given parameters.
    ///
    /// # Arguments
    ///
    /// * `rng`: the random number generator to use.
    /// * `threshold`: the number of generated bytes after which to reseed the RNG.
    /// * `reseeder`: the RNG to use for reseeding.
    pub fn new(rng: R, threshold: u64, reseeder: Rsdr) -> ReseedingRng<R,Rsdr> {
        assert!(threshold <= ::core::i64::MAX as u64);
        ReseedingRng {
            rng: rng,
            reseeder: reseeder,
            threshold: threshold as i64,
            bytes_until_reseed: threshold as i64,
        }
    }

    /// Reseed the internal PRNG.
    ///
    /// This will try to work around errors in the RNG used for reseeding
    /// intelligently through some combination of retrying and delaying
    /// reseeding until later. So long as the internal PRNG doesn't fail, this
    /// method will not fail, i.e. failures from the reseeding source are not
    /// fatal.
    pub fn reseed(&mut self) {
        // Behaviour is identical to `try_reseed`; we just squelch the error.
        let _res = self.try_reseed();
    }

    /// Reseed the internal RNG if the number of bytes that have been
    /// generated exceed the threshold.
    ///
    /// If reseeding fails, return an error with the original cause. Note that
    /// in case of error we simply delay reseeding, allowing the generator to
    /// continue its output of random data and try reseeding again later;
    /// because of this we always return kind `ErrorKind::Transient`.
    #[inline(never)]
    pub fn try_reseed(&mut self) -> Result<(), Error> {
        trace!("Reseeding RNG after {} generated bytes",
               self.threshold - self.bytes_until_reseed);
        if let Err(mut e) = R::from_rng(&mut self.reseeder)
                .map(|result| self.rng = result)
        {
            let delay = match e.kind {
                ErrorKind::Transient => 0,
                kind @ _ if kind.should_retry() => self.threshold >> 8,
                _ => self.threshold,
            };
            warn!("Reseeding RNG delayed reseeding by {} bytes due to \
                    error from source: {}", delay, e);
            self.bytes_until_reseed = delay;
            e.kind = ErrorKind::Transient;
            Err(e)
        } else {
            self.bytes_until_reseed = self.threshold;
            Ok(())
        }
    }
}

impl<R: RngCore + SeedableRng, Rsdr: RngCore> RngCore for ReseedingRng<R, Rsdr> {
    fn next_u32(&mut self) -> u32 {
        let value = self.rng.next_u32();
        self.bytes_until_reseed -= 4;
        if self.bytes_until_reseed <= 0 {
            self.reseed();
        }
        value
    }

    fn next_u64(&mut self) -> u64 {
        let value = self.rng.next_u64();
        self.bytes_until_reseed -= 8;
        if self.bytes_until_reseed <= 0 {
            self.reseed();
        }
        value
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.rng.fill_bytes(dest);
        self.bytes_until_reseed -= dest.len() as i64;
        if self.bytes_until_reseed <= 0 {
            self.reseed();
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
        let res1 = self.rng.try_fill_bytes(dest);
        self.bytes_until_reseed -= dest.len() as i64;
        let res2 = if self.bytes_until_reseed <= 0 {
            self.try_reseed()
        } else { Ok(()) };
        
        if let Err(e) = res1 {
            // In the unlikely event the internal PRNG fails, we don't know
            // whether this is resolvable; reseed immediately and return
            // original error kind.
            self.bytes_until_reseed = 0;
            Err(e)
        } else {
            res2
        }
    }
}

#[cfg(test)]
mod test {
    use {Rng, SeedableRng, StdRng};
    use mock::StepRng;
    use super::ReseedingRng;

    #[test]
    fn test_reseeding() {
        let mut zero = StepRng::new(0, 0);
        let rng = StdRng::from_rng(&mut zero).unwrap();
        let mut reseeding = ReseedingRng::new(rng, 32, zero);

        // Currently we only support for arrays up to length 32.
        // TODO: cannot generate seq via Rng::gen because it uses different alg
        let mut buf = [0u8; 32];
        reseeding.fill(&mut buf);
        let seq = buf;
        for _ in 0..10 {
            reseeding.fill(&mut buf);
            assert_eq!(buf, seq);
        }
    }
}
