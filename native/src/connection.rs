//! Socket lifecycle stays off the GUI thread. Every delivered event wakes egui.
use bone_client::SocketConn;
use bone_protocol::{RuntimeCommand, RuntimeEvent};
use eframe::egui;
use std::time::Duration;
use tokio::sync::mpsc;

pub enum Command {
    Connect(String),
    Disconnect,
    Send(RuntimeCommand),
}
pub enum Event {
    Connected,
    Disconnected(String),
    Runtime(RuntimeEvent),
}

async fn emit(events: &mpsc::Sender<Event>, ctx: &egui::Context, event: Event) -> bool {
    let sent = events.send(event).await.is_ok();
    ctx.request_repaint();
    sent
}

pub fn spawn(ctx: egui::Context) -> (mpsc::UnboundedSender<Command>, mpsc::Receiver<Event>) {
    let (commands, rx) = mpsc::unbounded_channel();
    let (tx, events) = mpsc::channel(256);
    // A small current-thread runtime per connection: the worker is one select
    // loop, and multi-conversation tabs must not multiply OS thread pools.
    std::thread::spawn(move || {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(worker(rx, tx, ctx)),
            Err(error) => {
                let _ = tx.blocking_send(Event::Disconnected(format!(
                    "Runtime initialization failed: {error}"
                )));
                ctx.request_repaint();
            }
        }
    });
    (commands, events)
}

async fn worker(
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::Sender<Event>,
    ctx: egui::Context,
) {
    while let Some(command) = commands.recv().await {
        let Command::Connect(address) = command else {
            continue;
        };
        let result = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect(&address)) => result,
            command = commands.recv() => {
                if command.is_none() { return; }
                if !emit(&events, &ctx, Event::Disconnected("Connection cancelled".into())).await { return; }
                continue;
            }
        };
        let stream = match result {
            Ok(Ok(stream)) => stream,
            error => {
                let message = match error {
                    Ok(Err(e)) => e.to_string(),
                    _ => "connection timed out after 10 seconds".into(),
                };
                if !emit(
                    &events,
                    &ctx,
                    Event::Disconnected(format!("Connect failed: {message}")),
                )
                .await
                {
                    return;
                }
                continue;
            }
        };
        let (read, write) = stream.into_split();
        let mut conn = SocketConn::new(read, write);
        let sender = conn.command_sender();
        if !emit(&events, &ctx, Event::Connected).await {
            return;
        }
        let reason = loop {
            tokio::select! {
                event = conn.next_event() => match event {
                    Some(event) => if !emit(&events, &ctx, Event::Runtime(event)).await { return; },
                    None => break "Connection lost. Delivery may be uncertain; reconnect manually. Prompts are never resent automatically.",
                },
                command = commands.recv() => match command {
                    Some(Command::Send(command)) => {
                        if sender.send(command).is_err() { break "Connection writer closed; delivery may be uncertain."; }
                    }
                    Some(Command::Disconnect) => break "Disconnected",
                    Some(Command::Connect(_)) => {}, // UI disables Connect while attached
                    None => return,
                }
            }
        };
        drop(conn);
        if !emit(&events, &ctx, Event::Disconnected(reason.into())).await {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loopback_commands_events_and_disconnect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (tx, rx) = mpsc::unbounded_channel();
        let (events, mut received) = mpsc::channel(256);
        let task = tokio::spawn(worker(rx, events, egui::Context::default()));
        tx.send(Command::Connect(address)).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        assert!(matches!(received.recv().await, Some(Event::Connected)));
        bone_client::write_message(
            &mut write,
            &RuntimeEvent::TextDelta {
                text: "hello".into(),
            },
        )
        .await
        .unwrap();
        assert!(
            matches!(received.recv().await, Some(Event::Runtime(RuntimeEvent::TextDelta { text })) if text == "hello")
        );
        tx.send(Command::Send(RuntimeCommand::Cancel)).unwrap();
        let mut reader = bone_client::MessageReader::new(read);
        assert!(matches!(
            reader.read::<RuntimeCommand>().await.unwrap(),
            Ok(RuntimeCommand::Cancel)
        ));
        tx.send(Command::Disconnect).unwrap();
        assert!(matches!(
            received.recv().await,
            Some(Event::Disconnected(_))
        ));
        assert!(
            tokio::time::timeout(Duration::from_secs(2), reader.read::<RuntimeCommand>())
                .await
                .unwrap()
                .is_none()
        );
        drop(tx);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }
}
