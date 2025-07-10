// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use std::time::{Duration, SystemTime};

/// Represents time as unsigned 64-bit milliseconds since the unix epoch.
/// Ain't no way I'm importing a whole dang time library for this.
///
/// This type can represent any time between 1970-01-01T00:00:00Z and 584556019-04-03T14:25:52Z.
/// Creating a SimpleTime earlier than the minimum will saturate, setting the SimpleTime to the unix epoch.
/// Creating a SimpleTime later than the maximum will wrap.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SimpleTime {
    unix_millis: u64,
}

impl SimpleTime {
    pub const UNIX_EPOCH: SimpleTime = SimpleTime::from_unix_millis(0);

    /// Create a SimpleTime from milliseconds since unix epoch.
    ///
    /// This is exactly equivalent to the internal representation, so this is a zero-cost wrap.
    #[inline(always)]
    pub const fn from_unix_millis(unix_millis: u64) -> Self {
        Self { unix_millis }
    }

    /// Convert this SimpleTime into milliseconds since unix epoch.
    ///
    /// This is exactly equivalent to the internal representation, so this is a zero-cost unwrap.
    #[inline(always)]
    pub const fn as_epoch_millis(self) -> u64 {
        self.unix_millis
    }

    /// Current time as per the system clock
    pub fn now() -> Self {
        let duration_since_epoch = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default(); // if the current time is before the unix epoch, then fuck you no it isn't. You get a zero.

        // This is ~very~ naughty truncating cast. This will break in a few hundred million years.
        // If my feeble human consciousness has been somehow been made immortal feel free to complain to me at that time.
        #[allow(clippy::cast_possible_truncation)]
        Self::from_unix_millis(duration_since_epoch.as_millis() as u64)
    }

    /// Duration since some earlier time with millisecond precision, or zero if result was negative
    #[inline(always)]
    pub fn duration_since(&self, earlier: Self) -> Duration {
        self.unix_millis
            .checked_sub(earlier.unix_millis)
            .map(Duration::from_millis)
            .unwrap_or_default()
    }

    /// Elapsed time since this SimpleTime and the present system clock time, or zero if result was negative.
    pub fn elapsed(&self) -> Duration {
        Self::now().duration_since(*self)
    }
}
