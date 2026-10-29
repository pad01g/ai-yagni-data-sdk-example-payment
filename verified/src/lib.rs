//! 決済サービスのうち、Verus で証明する部分: カードの利用枠の判定。
//!
//! 使った額 spent に amount を足しても枠 limit を超えないときだけ承認し、新しい使った額を返す。
//! 承認した額は 0 より大きく、足し算はあふれず、新しい使った額は枠を超えないことを証明する。
use vstd::prelude::*;

verus! {

pub fn authorize(spent: u64, amount: u64, limit: u64) -> (r: Option<u64>)
    ensures
        r matches Some(n) ==> n == spent + amount && n <= limit && amount > 0,
        r is None ==> amount == 0 || spent as int + amount as int > limit as int,
{
    if amount == 0 {
        return None;
    }
    if spent > limit || amount > limit - spent {
        return None;
    }
    Some(spent + amount)
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_within_the_limit() {
        assert_eq!(authorize(0, 100, 100), Some(100));
        assert_eq!(authorize(50, 51, 100), None);
        assert_eq!(authorize(0, 0, 100), None);
        assert_eq!(authorize(u64::MAX, 1, u64::MAX), None);
    }
}
