//! Status, time-in-force and order-type codes. Each keeps its raw `u8`, so a
//! newer file with codes this version does not know still reads.

use std::fmt;

/// The status of an ORDER record: what happened to the order.
///
/// The names are Hyperliquid's own (0 to 28; Hyperliquid adds statuses from
/// time to time, 24 to 28 appeared in 2026). 250 to 252 are written by
/// AlphaQuick's converter, not by Hyperliquid, where the node logs nothing for
/// a change the exchange made; the order's flags say why (see
/// [`Status::CORRECTION_RESTS`] and the `ORDER_*` flags in
/// [`crate::orderlog`]). Unknown codes are kept as they are; the converter
/// does not publish a day whose log holds a status it does not know.
///
/// ```
/// use alphaquick::orderlog::Status;
///
/// assert!(Status::CANCELED.removes_order());
/// assert!(Status::INSUFFICIENT_WEI_CANCELED.removes_order());
/// assert!(Status::PRIORITY_ALO_REDUCE_ONLY_REJECTED.is_reject());
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
    /// vaultWithdrawalCanceled: a vault's resting orders cancelled to free
    /// funds for a withdrawal. Removes the order.
    pub const VAULT_WITHDRAWAL_CANCELED: Status = Status(24);
    /// perpMaxPositionRejected (a reject).
    pub const PERP_MAX_POSITION_REJECTED: Status = Status(25);
    /// delistedCanceled: every resting order of a market cancelled when the
    /// market is delisted; open positions are closed in the same block by
    /// system prints ([`super::Trade::is_system_print`]). Removes the order.
    pub const DELISTED_CANCELED: Status = Status(26);
    /// insufficientWeiCanceled: a resting spot or HIP-4 order cancelled
    /// because the account no longer holds the token balance behind it
    /// (seen from November 2026). Removes the order.
    pub const INSUFFICIENT_WEI_CANCELED: Status = Status(27);
    /// priorityAloReduceOnlyRejected: a new reduce-only post-only order with a
    /// priority fee refused on arrival (seen from 17 November 2026). A reject.
    pub const PRIORITY_ALO_REDUCE_ONLY_REJECTED: Status = Status(28);
    /// Written by the converter: the order is gone; remove it.
    ///
    /// With [`super::ORDER_SNAPSHOT_FIX`], a correction against a node
    /// snapshot (in a BLOCK of its own). Without it, right after a reject of
    /// a triggered stop or take-profit order that a 251 had put in the book.
    pub const CORRECTION_GONE: Status = Status(250);
    /// Written by the converter: the order rests; add it at the back of its
    /// price. Most of these are not corrections:
    ///
    /// * with [`super::ORDER_REBUILT`]: an order of a Hyperliquid system
    ///   account (Hyperliquidity `0xffff…ffff`, `0xfefe…fefe`), whose
    ///   placement the node never logs, rebuilt from its later events and
    ///   written first in the block it was placed in (about 40,000 a day);
    /// * without [`super::ORDER_REBUILT`] or [`super::ORDER_SNAPSHOT_FIX`]: a
    ///   triggered stop or take-profit limit order that starts resting, right
    ///   after its `triggered` (Hyperliquid logs only that), with time in
    ///   force Gtc;
    /// * with [`super::ORDER_SNAPSHOT_FIX`]: a correction against a node
    ///   snapshot, in a BLOCK of its own.
    pub const CORRECTION_RESTS: Status = Status(251);
    /// Written by the converter: the order's remaining size; set it and keep
    /// the order's place in the queue.
    ///
    /// * with [`super::ORDER_RO_CAP`] (flags `0x11`): the exchange shrank a
    ///   resting reduce-only order so that the account's reduce-only orders
    ///   on that side total at most its position, which Hyperliquid does not
    ///   log. It comes after the block's trades and carries the size after
    ///   them, so apply the records in file order (about 120,000 to 180,000 a
    ///   day);
    /// * with [`super::ORDER_SNAPSHOT_FIX`]: a correction against a node
    ///   snapshot, in a BLOCK of its own; it can also increase a size.
    pub const CORRECTION_SIZE: Status = Status(252);
    /// A status the format has no code for.
    pub const OTHER: Status = Status(255);

    /// The raw code.
    pub const fn code(self) -> u8 {
        self.0
    }

    /// The status removes the order from the book: 1, 2, 9, 13, 15, 17, 18,
    /// 19, 20, 22, 24, 26, 27 and 250 ([`REMOVING_STATUSES`]).
    pub const fn removes_order(self) -> bool {
        matches!(
            self.0,
            1 | 2 | 9 | 13 | 15 | 17 | 18 | 19 | 20 | 22 | 24 | 26 | 27 | 250
        )
    }

    /// The status is a rejection: the order never rested. 3 to 8, 10 to 12,
    /// 16, 21, 23, 25 and 28 ([`REJECT_STATUSES`]).
    pub const fn is_reject(self) -> bool {
        matches!(self.0, 3..=8 | 10..=12 | 16 | 21 | 23 | 25 | 28)
    }

    /// Written by AlphaQuick's converter (250 to 252) rather than logged by
    /// Hyperliquid. Despite the name, only those flagged
    /// [`super::ORDER_SNAPSHOT_FIX`] are corrections against a node snapshot;
    /// most are regular (see [`Status::CORRECTION_RESTS`] and
    /// [`Status::CORRECTION_SIZE`]).
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
            24 => "vaultWithdrawalCanceled",
            25 => "perpMaxPositionRejected",
            26 => "delistedCanceled",
            27 => "insufficientWeiCanceled",
            28 => "priorityAloReduceOnlyRejected",
            250 => "correction: gone",
            251 => "correction: rests",
            252 => "correction: size",
            255 => "other",
            _ => return None,
        })
    }
}

/// The statuses that remove an order from the book, in code order: filled,
/// canceled, the other cancels, and the converter's 250. The same set as
/// [`Status::removes_order`].
pub const REMOVING_STATUSES: [Status; 14] = [
    Status::FILLED,
    Status::CANCELED,
    Status::REDUCE_ONLY_CANCELED,
    Status::SELF_TRADE_CANCELED,
    Status::SIBLING_FILLED_CANCELED,
    Status::OUTCOME_SETTLED_CANCELED,
    Status::SCHEDULED_CANCEL,
    Status::MARGIN_CANCELED,
    Status::LIQUIDATED_CANCELED,
    Status::OPEN_INTEREST_CAP_CANCELED,
    Status::VAULT_WITHDRAWAL_CANCELED,
    Status::DELISTED_CANCELED,
    Status::INSUFFICIENT_WEI_CANCELED,
    Status::CORRECTION_GONE,
];

/// The statuses of a rejected order, which never touched the book, in code
/// order. The same set as [`Status::is_reject`].
pub const REJECT_STATUSES: [Status; 14] = [
    Status::BAD_ALO_PX_REJECTED,
    Status::PERP_MARGIN_REJECTED,
    Status::MIN_TRADE_NTL_REJECTED,
    Status::IOC_CANCEL_REJECTED,
    Status::TOO_MANY_OPEN_ORDERS_REJECTED,
    Status::INSUFFICIENT_SPOT_BALANCE_REJECTED,
    Status::REDUCE_ONLY_REJECTED,
    Status::ORACLE_REJECTED,
    Status::POSITION_INCREASE_AT_OPEN_INTEREST_CAP_REJECTED,
    Status::POSITION_FLIP_AT_OPEN_INTEREST_CAP_REJECTED,
    Status::TOO_AGGRESSIVE_AT_OPEN_INTEREST_CAP_REJECTED,
    Status::OPEN_INTEREST_INCREASE_REJECTED,
    Status::PERP_MAX_POSITION_REJECTED,
    Status::PRIORITY_ALO_REDUCE_ONLY_REJECTED,
];

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

    /// The writer's sets (alphaquick-rs `hl_convert/hlc/src/records.rs`,
    /// `status_removes` and `status_is_reject`), by code.
    const WRITER_REMOVES: [u8; 14] = [1, 2, 9, 13, 15, 17, 18, 19, 20, 22, 24, 26, 27, 250];
    const WRITER_REJECTS: [u8; 14] = [3, 4, 5, 6, 7, 8, 10, 11, 12, 16, 21, 23, 25, 28];

    #[test]
    fn statuses_are_grouped_as_the_format_says() {
        let removing: Vec<u8> = (0..=255).filter(|&c| Status(c).removes_order()).collect();
        assert_eq!(removing, WRITER_REMOVES);
        let rejects: Vec<u8> = (0..=255).filter(|&c| Status(c).is_reject()).collect();
        assert_eq!(rejects, WRITER_REJECTS);
        let listed: Vec<u8> = REMOVING_STATUSES.iter().map(|s| s.code()).collect();
        assert_eq!(listed, WRITER_REMOVES);
        let listed: Vec<u8> = REJECT_STATUSES.iter().map(|s| s.code()).collect();
        assert_eq!(listed, WRITER_REJECTS);
        for code in 0..=28 {
            let status = Status(code);
            assert!(status.name().is_some(), "{code}");
            assert_eq!(
                status.removes_order(),
                WRITER_REMOVES.contains(&code),
                "{code}"
            );
            assert_eq!(status.is_reject(), WRITER_REJECTS.contains(&code), "{code}");
            // Only open (0) and triggered (14) are neither.
            assert_eq!(
                !status.removes_order() && !status.is_reject(),
                code == 0 || code == 14,
                "{code}"
            );
        }
        assert_eq!(Status(29).name(), None);
        assert_eq!(
            Status::INSUFFICIENT_WEI_CANCELED.to_string(),
            "insufficientWeiCanceled"
        );
        assert_eq!(Status(28).to_string(), "priorityAloReduceOnlyRejected");
        assert_eq!(Status(77).to_string(), "status 77");
        assert_eq!(Status::CORRECTION_GONE.to_string(), "correction: gone");
        assert!(Status::CORRECTION_SIZE.is_correction() && !Status::OTHER.is_correction());
        assert_eq!(Tif::ALO.to_string(), "Alo");
        assert_eq!(Tif(9).to_string(), "tif 9");
        assert_eq!(OrderType::STOP_LIMIT.to_string(), "Stop Limit");
        assert_eq!(OrderType(9).code(), 9);
    }
}
