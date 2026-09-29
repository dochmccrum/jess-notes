//! Hybrid logical clocks (DESIGN §6.1).

use minicbor::{Decode, Encode};

/// `(wall_ms: u48, logical: u16, replica_id: u64)`, totally ordered lexicographically.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Debug, Encode, Decode)]
#[cbor(array)]
pub struct Hlc {
    #[n(0)]
    pub wall: u64,
    #[n(1)]
    pub logical: u16,
    #[n(2)]
    pub replica: u64,
}

pub const MAX_WALL: u64 = (1 << 48) - 1;

impl Hlc {
    pub const ZERO: Hlc = Hlc {
        wall: 0,
        logical: 0,
        replica: 0,
    };
    pub fn new(wall: u64, logical: u16, replica: u64) -> Hlc {
        Hlc {
            wall: wall.min(MAX_WALL),
            logical,
            replica,
        }
    }
}

/// A replica's clock. `now` issues strictly increasing stamps; `observe` merges remote stamps
/// so a write made after seeing another write always orders after it.
#[derive(Clone, Debug)]
pub struct Clock {
    pub replica: u64,
    pub last: Hlc,
}

impl Clock {
    pub fn new(replica: u64) -> Clock {
        Clock {
            replica,
            last: Hlc {
                wall: 0,
                logical: 0,
                replica,
            },
        }
    }
    pub fn now(&mut self, phys_ms: u64) -> Hlc {
        let phys = phys_ms.min(MAX_WALL);
        let (wall, logical) = if phys > self.last.wall {
            (phys, 0)
        } else if self.last.logical == u16::MAX {
            (self.last.wall + 1, 0)
        } else {
            (self.last.wall, self.last.logical + 1)
        };
        self.last = Hlc {
            wall,
            logical,
            replica: self.replica,
        };
        self.last
    }
    pub fn observe(&mut self, remote: Hlc) {
        if (remote.wall, remote.logical) > (self.last.wall, self.last.logical) {
            self.last = Hlc {
                wall: remote.wall.min(MAX_WALL),
                logical: remote.logical,
                replica: self.replica,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn monotonic_and_causal() {
        let mut a = Clock::new(1);
        let mut b = Clock::new(2);
        let x = a.now(1000);
        let y = a.now(900); // clock went backwards
        assert!(y > x);
        b.observe(y);
        let z = b.now(10); // b's clock is far behind
        assert!(z > y, "a write made after seeing another write wins");
    }
}
