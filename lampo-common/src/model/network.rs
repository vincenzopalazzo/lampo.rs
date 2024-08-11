pub mod request {}

pub mod response {
    use lightning::routing::gossip::ChannelInfo;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, Clone)]
    pub struct NetworkChannels {
        pub channels: Vec<NetworkChannel>,
    }

    #[derive(Clone, Serialize, Deserialize, Debug)]
    pub struct NetworkChannel {
        pub node_one: String,
        pub node_two: String,
    }

    impl From<ChannelInfo> for NetworkChannel {
        fn from(value: ChannelInfo) -> Self {
            Self {
                node_one: value.node_one.to_string(),
                node_two: value.node_two.to_string(),
            }
        }
    }
}
