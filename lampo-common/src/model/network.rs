pub mod request {}

pub mod response {
    use lightning::routing::gossip::ChannelInfo;
    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, Clone, Apiv2Schema)]
    pub struct NetworkChannels {
        pub channels: Vec<NetworkChannel>,
    }

    #[derive(Clone, Serialize, Deserialize, Debug, Apiv2Schema)]
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
