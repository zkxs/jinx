// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use std::time::{Duration, SystemTime};

/// This just represents time as unsigned 64-bit ms since unix epoch. Ain't no way I'm importing a whole dang time library for this.
#[derive(Copy, Clone)]
pub struct SimpleTime {
    unix_millis: u64,
}

impl SimpleTime {
    #[inline(always)]
    pub fn from_unix_millis(unix_millis: u64) -> Self {
        Self { unix_millis }
    }

    #[inline(always)]
    pub fn as_epoch_millis(&self) -> u64 {
        self.unix_millis
    }

    /// Current time as per the system clock
    pub fn now() -> Self {
        let duration_since_epoch = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default(); // if the current time is before the unix epoch, then fuck you no it isn't. You get a zero.
        Self::from_unix_millis(duration_since_epoch.as_millis() as u64) // a very naughty truncating cast. This will break in a few hundred million years. If my feeble human consciousness has been somehow been made immortal feel free to complain to me at that time.
    }

    /// Duration since some earlier time with millisecond precision, or None if result was negative
    #[inline(always)]
    pub fn duration_since(&self, earlier: Self) -> Option<Duration> {
        self.unix_millis
            .checked_sub(earlier.unix_millis)
            .map(Duration::from_millis)
    }

    /// Elapsed time since this SimpleTime and the present system clock time, or None if result was negative.
    pub fn elapsed(&self) -> Option<Duration> {
        Self::now().duration_since(*self)
    }
}
