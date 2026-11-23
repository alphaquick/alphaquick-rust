//! Status, time-in-force and order-type codes. Each keeps its raw `u8`, so a
//! newer file with codes this version does not know still reads.

use std::fmt;

/// The status of an ORDER record: what happened to the order.
///
/// The names are Hyperliquid's own; 250 to 252 are corrections written by
/// AlphaQuick's converter. Unknown codes are kept as they are.
///
/// ```
/// use alphaquick::orderlog::Status;
///
/// assert!(Status::CANCELED.removes_order());
/// assert_eq!(Status::CANCELED.to_string(), "canceled");
/// assert_eq!(Status(77).to_string(), "status 77");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Status(pub u8);

/// Time in force of an order. [`Tif::NONE`] marks a trigger order (a stop or
/// a take-profit) that waits off the book until it triggers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Tif(pub u8);

/// The order type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OrderType(pub u8);

impl Status {
    /// open
    pub const OPEN: Status = Status(0);
    /// filled
    pub const FILLED: Status = Status(1);
    /// canceled
    pub const CANCELED: Status = Status(2);
    /// badAloPxRejected
    pub const BAD_ALO_PX_REJECTED: Status = Status(3);
    /// perpMarginRejected
    pub const PERP_MARGIN_REJECTED: Status = Status(4);
    /// minTradeNtlRejected
    pub const MIN_TRADE_NTL_REJECTED: Status = Status(5);
    /// iocCancelRejected
    pub const IOC_CANCEL_REJECTED: Status = Status(6);
    /// tooManyOpenOrdersRejected
    pub const TOO_MANY_OPEN_ORDERS_REJECTED: Status = Status(7);
    /// insufficientSpotBalanceRejected
    pub const INSUFFICIENT_SPOT_BALANCE_REJECTED: Status = Status(8);
    /// reduceOnlyCanceled
    pub const REDUCE_ONLY_CANCELED: Status = Status(9);
    /// reduceOnlyRejected
    pub const REDUCE_ONLY_REJECTED: Status = Status(10);
    /// oracleRejected
    pub const ORACLE_REJECTED: Status = Status(11);
    /// positionIncreaseAtOpenInterestCapRejected
    pub const POSITION_INCREASE_AT_OPEN_INTEREST_CAP_REJECTED: Status = Status(12);
    /// selfTradeCanceled
    pub const SELF_TRADE_CANCELED: Status = Status(13);
    /// triggered
    pub const TRIGGERED: Status = Status(14);
    /// siblingFilledCanceled
    pub const SIBLING_FILLED_CANCELED: Status = Status(15);
    /// positionFlipAtOpenInterestCapRejected
    pub const POSITION_FLIP_AT_OPEN_INTEREST_CAP_REJECTED: Status = Status(16);
    /// outcomeSettledCanceled
    pub const OUTCOME_SETTLED_CANCELED: Status = Status(17);
    /// scheduledCancel
    pub const SCHEDULED_CANCEL: Status = Status(18);
    /// marginCanceled
    pub const MARGIN_CANCELED: Status = Status(19);
    /// liquidatedCanceled
    pub const LIQUIDATED_CANCELED: Status = Status(20);
    /// tooAggressiveAtOpenInterestCapRejected
    pub const TOO_AGGRESSIVE_AT_OPEN_INTEREST_CAP_REJECTED: Status = Status(21);
    /// openInterestCapCanceled
    pub const OPEN_INTEREST_CAP_CANCELED: Status = Status(22);
    /// openInterestIncreaseRejected
    pub const OPEN_INTEREST_INCREASE_REJECTED: Status = Status(23);
    /// Correction by the converter: the order is gone.
    pub const CORRECTION_GONE: Status = Status(250);
    /// Correction by the converter: the order rests.
    pub const CORRECTION_RESTS: Status = Status(251);
    /// Correction by the converter: the order's remaining size.
    pub const CORRECTION_SIZE: Status = Status(252);
    /// A status the format has no code for.
    pub const OTHER: Status = Status(255);

    /// The raw code.
    pub const fn code(self) -> u8 {
        self.0
    }

    /// The status removes the order from the book: 1, 2, 9, 13, 15, 17, 18,
    /// 19, 20, 22 and 250.
    pub const fn removes_order(self) -> bool {
        matches!(self.0, 1 | 2 | 9 | 13 | 15 | 17 | 18 | 19 | 20 | 22 | 250)
    }

    /// The status is a rejection: the order never rested.
    pub const fn is_reject(self) -> bool {
        matches!(self.0, 3..=8 | 10..=12 | 16 | 21 | 23)
    }

    /// A correction written by the converter (250 to 252).
    pub const fn is_correction(self) -> bool {
        matches!(self.0, 250..=252)
    }

    /// Hyperliquid's name of the status (`"canceled"`), the correction's
    /// (`"correction: gone"`), or `None` for a code this version does not know.
    pub const fn name(self) -> Option<&'static str> {
        Some(match self.0 {
            0 => "open",
            1 => "filled",
            2 => "canceled",
            3 => "badAloPxRejected",
            4 => "perpMarginRejected",
            5 => "minTradeNtlRejected",
            6 => "iocCancelRejected",
            7 => "tooManyOpenOrdersRejected",
            8 => "insufficientSpotBalanceRejected",
            9 => "reduceOnlyCanceled",
            10 => "reduceOnlyRejected",
            11 => "oracleRejected",
            12 => "positionIncreaseAtOpenInterestCapRejected",
            13 => "selfTradeCanceled",
            14 => "triggered",
            15 => "siblingFilledCanceled",
            16 => "positionFlipAtOpenInterestCapRejected",
            17 => "outcomeSettledCanceled",
            18 => "scheduledCancel",
            19 => "marginCanceled",
            20 => "liquidatedCanceled",
            21 => "tooAggressiveAtOpenInterestCapRejected",
            22 => "openInterestCapCanceled",
            23 => "openInterestIncreaseRejected",
            250 => "correction: gone",
            251 => "correction: rests",
            252 => "correction: size",
            255 => "other",
            _ => return None,
        })
    }
}

impl Tif {
    /// None: a trigger order, waiting off the book.
    pub const NONE: Tif = Tif(0);
    /// Good till canceled.
    pub const GTC: Tif = Tif(1);
    /// Add liquidity only (post-only).
    pub const ALO: Tif = Tif(2);
    /// Immediate or cancel.
    pub const IOC: Tif = Tif(3);
    /// A market order from Hyperliquid's app.
    pub const FRONTEND_MARKET: Tif = Tif(4);
    /// A liquidation.
    pub const LIQUIDATION_MARKET: Tif = Tif(5);
    /// A time in force the format has no code for.
    pub const OTHER: Tif = Tif(255);

    /// The raw code.
    pub const fn code(self) -> u8 {
        self.0
    }

    /// The format's name (`"Gtc"`), or `None` for an unknown code.
    pub const fn name(self) -> Option<&'static str> {
        Some(match self.0 {
            0 => "none",
            1 => "Gtc",
            2 => "Alo",
            3 => "Ioc",
            4 => "FrontendMarket",
            5 => "LiquidationMarket",
            255 => "other",
            _ => return None,
        })
    }
}

impl OrderType {
    /// Limit.
    pub const LIMIT: OrderType = OrderType(0);
    /// Market.
    pub const MARKET: OrderType = OrderType(1);
    /// Stop Market.
    pub const STOP_MARKET: OrderType = OrderType(2);
    /// Stop Limit.
    pub const STOP_LIMIT: OrderType = OrderType(3);
    /// Take Profit Market.
    pub const TAKE_PROFIT_MARKET: OrderType = OrderType(4);
    /// Take Profit Limit.
    pub const TAKE_PROFIT_LIMIT: OrderType = OrderType(5);
    /// An order type the format has no code for.
    pub const OTHER: OrderType = OrderType(255);

    /// The raw code.
    pub const fn code(self) -> u8 {
        self.0
    }

    /// The format's name (`"Stop Market"`), or `None` for an unknown code.
    pub const fn name(self) -> Option<&'static str> {
        Some(match self.0 {
            0 => "Limit",
            1 => "Market",
            2 => "Stop Market",
            3 => "Stop Limit",
            4 => "Take Profit Market",
            5 => "Take Profit Limit",
            255 => "other",
            _ => return None,
        })
    }
}

macro_rules! display_by_name {
    ($type:ty, $prefix:literal) => {
        impl fmt::Display for $type {
            /// The name, or the prefix and the raw code for a code this version does not know.
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self.name() {
                    Some(name) => f.write_str(name),
                    None => write!(f, concat!($prefix, " {}"), self.0),
                }
            }
        }
    };
}

display_by_name!(Status, "status");
display_by_name!(Tif, "tif");
display_by_name!(OrderType, "order type");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_are_grouped_as_the_format_says() {
        let removing: Vec<u8> = (0..=255).filter(|&c| Status(c).removes_order()).collect();
        assert_eq!(removing, vec![1, 2, 9, 13, 15, 17, 18, 19, 20, 22, 250]);
        let rejects: Vec<u8> = (0..=255).filter(|&c| Status(c).is_reject()).collect();
        assert_eq!(rejects, vec![3, 4, 5, 6, 7, 8, 10, 11, 12, 16, 21, 23]);
        for code in 0..=23 {
            assert!(Status(code).name().is_some());
        }
        assert_eq!(Status(77).to_string(), "status 77");
        assert_eq!(Status::CORRECTION_GONE.to_string(), "correction: gone");
        assert!(Status::CORRECTION_SIZE.is_correction() && !Status::OTHER.is_correction());
        assert_eq!(Tif::ALO.to_string(), "Alo");
        assert_eq!(Tif(9).to_string(), "tif 9");
        assert_eq!(OrderType::STOP_LIMIT.to_string(), "Stop Limit");
        assert_eq!(OrderType(9).code(), 9);
    }
}
