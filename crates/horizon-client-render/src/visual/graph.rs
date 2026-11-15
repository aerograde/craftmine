use super::Tier;

/// The passes the built-in visual profile runs this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PassSet {
    pub shadow: bool,
    pub bloom: bool,
    pub shafts: bool,
}

pub fn passes(tier: Tier, visuals: bool, pack_owns_post: bool) -> PassSet {
    if !visuals || pack_owns_post {
        return PassSet::default();
    }
    let plan = tier.shadow();
    PassSet {
        shadow: plan.resolution > 0,
        // The pack has neither bloom nor light shafts.
        bloom: false,
        shafts: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_keeps_the_grade_without_the_extra_passes() {
        assert_eq!(
            passes(Tier::Low, true, false),
            PassSet {
                shadow: true,
                bloom: false,
                shafts: false
            }
        );
    }

    #[test]
    fn a_pack_or_visuals_off_owns_every_pass() {
        assert_eq!(passes(Tier::High, false, false), PassSet::default());
        assert_eq!(passes(Tier::High, true, true), PassSet::default());
    }

    #[test]
    fn high_and_medium_follow_the_tier() {
        assert_eq!(
            passes(Tier::High, true, false),
            PassSet {
                shadow: true,
                bloom: false,
                shafts: false
            }
        );
        assert_eq!(
            passes(Tier::Medium, true, false),
            PassSet {
                shadow: true,
                bloom: false,
                shafts: false
            }
        );
    }
}
