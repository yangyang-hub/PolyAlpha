use rust_decimal::Decimal;

/// Floor a price to the nearest valid market tick.
pub(crate) fn floor_price_to_tick(price: Decimal, tick: Decimal) -> Decimal {
    if tick <= Decimal::ZERO {
        return price.round_dp(2);
    }
    (price / tick).floor() * tick
}

#[cfg(test)]
mod tests {
    use super::floor_price_to_tick;
    use rust_decimal_macros::dec;

    #[test]
    fn test_floor_price_to_tick_obeys_market_tick_size() {
        assert_eq!(floor_price_to_tick(dec!(0.995), dec!(0.01)), dec!(0.99));
        assert_eq!(floor_price_to_tick(dec!(0.611), dec!(0.001)), dec!(0.611));
        assert_eq!(floor_price_to_tick(dec!(0.6119), dec!(0.001)), dec!(0.611));
    }
}
