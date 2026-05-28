use alloy_genesis::Genesis;
use std::sync::LazyLock;

pub static AENEID_GENESIS: LazyLock<Genesis> = LazyLock::new(|| {
    serde_json::from_str(include_str!("chainspecs/aeneid.json"))
        .expect("Can't deserialize Mainnet genesis json")
});

pub static STORY_GENESIS: LazyLock<Genesis> = LazyLock::new(|| {
    serde_json::from_str(include_str!("chainspecs/story.json"))
        .expect("Can't deserialize Mainnet genesis json")
});
