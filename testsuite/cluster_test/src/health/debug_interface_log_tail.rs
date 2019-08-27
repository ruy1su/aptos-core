use crate::{
    cluster::Cluster,
    health::{Commit, Event, LogTail, ValidatorEvent},
    instance::Instance,
    util::unix_timestamp_now,
};
use debug_interface::{
    self,
    proto::{
        node_debug_interface::{Event as DebugInterfaceEvent, GetEventsRequest},
        node_debug_interface_grpc::NodeDebugInterfaceClient,
    },
};
use grpcio::{self, ChannelBuilder, EnvBuilder};
use serde_json::{self, value as json};
use std::{
    env,
    sync::{atomic::AtomicI64, mpsc, Arc},
    thread,
    time::Duration,
};

pub struct DebugPortLogThread {
    instance: Instance,
    client: NodeDebugInterfaceClient,
    event_sender: mpsc::Sender<ValidatorEvent>,
}

impl DebugPortLogThread {
    pub fn spawn_new(cluster: &Cluster) -> LogTail {
        let (event_sender, event_receiver) = mpsc::channel();
        let env = Arc::new(EnvBuilder::new().name_prefix("grpc-log-tail-").build());
        for instance in cluster.instances() {
            let ch =
                ChannelBuilder::new(env.clone()).connect(&format!("{}:{}", instance.ip(), 6191));
            let client = NodeDebugInterfaceClient::new(ch);
            let debug_port_log_thread = DebugPortLogThread {
                instance: instance.clone(),
                client,
                event_sender: event_sender.clone(),
            };
            thread::Builder::new()
                .name(format!("log-tail-{}", instance.short_hash()))
                .spawn(move || debug_port_log_thread.run())
                .expect("Failed to spawn log tail thread");
        }
        LogTail {
            event_receiver,
            pending_messages: Arc::new(AtomicI64::new(0)),
        }
    }
}

impl DebugPortLogThread {
    pub fn run(self) {
        let print_failures = env::var("VERBOSE").is_ok();
        loop {
            let opts = grpcio::CallOption::default().timeout(Duration::from_secs(5));
            match self.client.get_events_opt(&GetEventsRequest::new(), opts) {
                Err(e) => {
                    if print_failures {
                        println!("Failed to get events from {}: {:?}", self.instance, e);
                    }
                    thread::sleep(Duration::from_secs(1));
                }
                Ok(resp) => {
                    for event in resp.events.into_iter() {
                        if let Some(e) = self.parse_event(event) {
                            let _ignore = self.event_sender.send(e);
                        }
                    }
                    thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    fn parse_event(&self, event: DebugInterfaceEvent) -> Option<ValidatorEvent> {
        let json: json::Value =
            serde_json::from_str(&event.json).expect("Failed to parse json from debug interface");

        let e = if event.name == "committed" {
            Self::parse_commit(json)
        } else {
            println!("Unknown event: {} from {}", event.name, self.instance);
            return None;
        };
        Some(ValidatorEvent {
            validator: self.instance.short_hash().clone(),
            timestamp: Duration::from_millis(event.get_timestamp() as u64),
            received_timestamp: unix_timestamp_now(),
            event: e,
        })
    }

    fn parse_commit(json: json::Value) -> Event {
        Event::Commit(Commit {
            commit: json
                .get("block_id")
                .expect("No block_id in commit event")
                .as_str()
                .expect("block_id is not string")
                .to_string(),
            round: json
                .get("round")
                .expect("No round in commit event")
                .as_u64()
                .expect("round is not u64"),
            parent: json
                .get("parent_id")
                .expect("No parent_id in commit event")
                .as_str()
                .expect("parent_id is not string")
                .to_string(),
        })
    }
}
