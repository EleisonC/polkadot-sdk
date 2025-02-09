// This file is part of Substrate.

// Copyright (C) Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use futures::{channel::oneshot, StreamExt};
use sc_network_types::PeerId;

use sc_network::{
	request_responses::{IfDisconnected, RequestFailure},
	types::ProtocolName,
	NetworkPeers, NetworkRequest, ReputationChange,
};
use sc_utils::mpsc::{tracing_unbounded, TracingUnboundedReceiver, TracingUnboundedSender};

use std::sync::Arc;

/// Network-related services required by `sc-network-sync`
pub trait Network: NetworkPeers + NetworkRequest {}

impl<T> Network for T where T: NetworkPeers + NetworkRequest {}

/// Network service provider for `ChainSync`
///
/// It runs as an asynchronous task and listens to commands coming from `ChainSync` and
/// calls the `NetworkService` on its behalf.
pub struct NetworkServiceProvider {
	rx: TracingUnboundedReceiver<ToServiceCommand>,
	handle: NetworkServiceHandle,
}

/// Commands that `ChainSync` wishes to send to `NetworkService`
#[derive(Debug)]
pub enum ToServiceCommand {
	/// Call `NetworkPeers::disconnect_peer()`
	DisconnectPeer(PeerId, ProtocolName),

	/// Call `NetworkPeers::report_peer()`
	ReportPeer(PeerId, ReputationChange),

	/// Call `NetworkRequest::start_request()`
	StartRequest(
		PeerId,
		ProtocolName,
		Vec<u8>,
		oneshot::Sender<Result<(Vec<u8>, ProtocolName), RequestFailure>>,
		IfDisconnected,
	),
}

/// Handle that is (temporarily) passed to `ChainSync` so it can
/// communicate with `NetworkService` through `SyncingEngine`
#[derive(Debug, Clone)]
pub struct NetworkServiceHandle {
	tx: TracingUnboundedSender<ToServiceCommand>,
}

impl NetworkServiceHandle {
	/// Create new service handle
	pub fn new(tx: TracingUnboundedSender<ToServiceCommand>) -> NetworkServiceHandle {
		Self { tx }
	}

	/// Report peer
	pub fn report_peer(&self, who: PeerId, cost_benefit: ReputationChange) {
		let _ = self.tx.unbounded_send(ToServiceCommand::ReportPeer(who, cost_benefit));
	}

	/// Disconnect peer
	pub fn disconnect_peer(&self, who: PeerId, protocol: ProtocolName) {
		let _ = self.tx.unbounded_send(ToServiceCommand::DisconnectPeer(who, protocol));
	}

	/// Send request to peer
	pub fn start_request(
		&self,
		who: PeerId,
		protocol: ProtocolName,
		request: Vec<u8>,
		tx: oneshot::Sender<Result<(Vec<u8>, ProtocolName), RequestFailure>>,
		connect: IfDisconnected,
	) {
		let _ = self
			.tx
			.unbounded_send(ToServiceCommand::StartRequest(who, protocol, request, tx, connect));
	}
}

impl NetworkServiceProvider {
	/// Create new `NetworkServiceProvider`
	pub fn new() -> Self {
		let (tx, rx) = tracing_unbounded("mpsc_network_service_provider", 100_000);

		Self { rx, handle: NetworkServiceHandle::new(tx) }
	}

	/// Get handle to talk to the provider
	pub fn handle(&self) -> NetworkServiceHandle {
		self.handle.clone()
	}

	/// Run the `NetworkServiceProvider`
	pub async fn run(self, service: Arc<dyn Network + Send + Sync>) {
		let Self { mut rx, handle } = self;
		drop(handle);

		while let Some(inner) = rx.next().await {
			match inner {
				ToServiceCommand::DisconnectPeer(peer, protocol_name) =>
					service.disconnect_peer(peer, protocol_name),
				ToServiceCommand::ReportPeer(peer, reputation_change) =>
					service.report_peer(peer, reputation_change),
				ToServiceCommand::StartRequest(peer, protocol, request, tx, connect) =>
					service.start_request(peer, protocol, request, None, tx, connect),
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::service::mock::MockNetwork;

	// typical pattern in `Protocol` code where peer is disconnected
	// and then reported
	#[tokio::test]
	async fn disconnect_and_report_peer() {
		let provider = NetworkServiceProvider::new();
		let handle = provider.handle();

		let peer = PeerId::random();
		let proto = ProtocolName::from("test-protocol");
		let proto_clone = proto.clone();
		let change = sc_network::ReputationChange::new_fatal("test-change");

		let mut mock_network = MockNetwork::new();
		mock_network
			.expect_disconnect_peer()
			.withf(move |in_peer, in_proto| &peer == in_peer && &proto == in_proto)
			.once()
			.returning(|_, _| ());
		mock_network
			.expect_report_peer()
			.withf(move |in_peer, in_change| &peer == in_peer && &change == in_change)
			.once()
			.returning(|_, _| ());

		tokio::spawn(async move {
			provider.run(Arc::new(mock_network)).await;
		});

		handle.disconnect_peer(peer, proto_clone);
		handle.report_peer(peer, change);
	}
}
