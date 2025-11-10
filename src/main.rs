use std::collections::HashMap;
use std::net::SocketAddr;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

type Message = String;

type ClientTx = mpsc::UnboundedSender<Message>;
type ServerCommand = (Message, ClientTx, SocketAddr); // Broker expects the message AND the sender
type BrokerRx = mpsc::UnboundedReceiver<ServerCommand>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:8080").await?;
    println!("Chat server running on 127.0.0.1:8080");

    // Channel for broadcasting messages to all clients
    let (tx_broker, rx_broker) = mpsc::unbounded_channel();

    // Spawn the broker task
    tokio::spawn(server_broker(rx_broker));
    loop {
        let (socket, addr) = listener.accept().await?;
        println!("New client connected: {}", addr);

        // Clone the sender for this client
        let tx_broker_clone = tx_broker.clone();

        // Spawn a new async task for each client
        tokio::spawn(async move {
            if let Err(e) = handle_client(socket, addr, tx_broker_clone).await {
                eprintln!("Error handling client {}: {}", addr, e);
            }
        });
    }
}

// The handle_client function signature matching your main loop.
// It receives the client's socket and the central sender channel.
async fn handle_client(
    socket: TcpStream,
    addr: SocketAddr,
    server_tx: mpsc::UnboundedSender<ServerCommand>, // The central channel for sending messages *to* the broker
) -> anyhow::Result<()> {
    // --- 1. SETUP: Prepare for concurrent Read and Write ---

    // Create a dedicated channel for the server to send messages *to* this client.
    // For simplicity and efficiency in a chat server, we'll use an Unbounded channel
    // for Server -> Client messages, as we don't want the server to block when sending
    // broadcasts. We'll define the ClientTx type here.
    // type ClientTx = mpsc::UnboundedSender<Message>;

    // Create the channel pair for Server -> Client
    let (client_tx, mut client_rx) = mpsc::unbounded_channel::<Message>();

    // *** NOTE: Due to the main function's simplicity, the central server_tx
    //           doesn't currently know how to BROADCAST to all clients.
    //           In a complete system, we'd need a central broker task
    //           to manage a HashMap<Addr, ClientTx>. ***
    //
    //           Since we're only writing *this* function, we'll assume the
    //           central logic will handle the broadcast based on the messages
    //           received by server_tx.
    //
    //
    // Clone necessary variables for the read task
    let client_tx_clone_for_read_task = client_tx.clone();
    let server_tx_clone = server_tx.clone(); // If server_tx is used elsewhere

    // --- 1. Registration Command (Must be sent first) ---
    // You need to send a registration message *before* the read_task loop starts.
    // Assuming "REGISTER" is the message to signal a new client.
    let _ = server_tx_clone.send(("REGISTER".to_string(), client_tx.clone(), addr));

    // Split the stream into owned halves to enable concurrent read/write tasks (Fixes try_clone())
    let (reader_stream, mut writer) = socket.into_split();

    let mut reader = BufReader::new(reader_stream);
    let mut line = String::new();

    // --- 2. READER TASK (Client -> Server) ---
    // Reads data from the client's socket and forwards it to the central server_tx.
    let read_task = tokio::spawn(async move {
        // Use the moved/cloned variables here:
        let server_tx = server_tx_clone;
        let client_tx = client_tx_clone_for_read_task; // The dedicated sender for this client
        // let mut buf = vec![0; 1024];
        loop {
            // Read data from the client
            match reader.read_line(&mut line).await {
                Ok(0) => break, // Connection closed gracefully
                Ok(_n) => {
                    // Convert bytes to string (assuming UTF-8)
                    let msg = line.trim().to_string();

                    if !msg.is_empty() {
                        // Send the received message to the central server channel
                        // We clone the message because the String is non-Copy and needs to be moved.
                        if server_tx
                            .send((msg.clone(), client_tx.clone(), addr))
                            .is_err()
                        {
                            // Central server's receiver is dropped; shut down this task
                            break;
                        }
                    }
                    line.clear();
                }
                Err(e) => {
                    eprintln!("Failed to read from socket: {}", e);
                    break;
                }
            };
        }
        // Return Ok(()) on clean shutdown
        Ok::<(), anyhow::Error>(())
    });

    // --- 3. WRITER TASK (Server -> Client) ---
    // Reads broadcast messages from client_rx and writes them to the client's socket.

    let write_task = tokio::spawn(async move {
        while let Some(msg) = client_rx.recv().await {
            // Write the broadcasted message to the client
            if writer.write_all(msg.as_bytes()).await.is_err() {
                // Client socket write failed (client disconnected forcefully)
                break;
            }
            writer.flush().await?;
        }
        // Return Ok(()) on clean shutdown
        Ok::<(), anyhow::Error>(())
    });

    // --- 4. LIFECYCLE MANAGEMENT ---

    // Wait for either the read or write task to finish.
    // If one fails, the connection is considered dead.
    tokio::select! {
        // If the read task finishes (client disconnected), shut down the writer.
        res = read_task => {
            if let Err(e) = res? {
                // If the task failed internally
                return Err(e.into());
            }
        },
        // If the write task finishes (e.g., failed write), shut down the reader.
        res = write_task => {
            if let Err(e) = res? {
                // If the task failed internally
                return Err(e.into());
            }
        }
    }

    Ok(())
}

async fn server_broker(mut rx: BrokerRx) {
    let mut clients: HashMap<SocketAddr, ClientTx> = HashMap::new();

    while let Some((msg, client_tx, sender_addr)) = rx.recv().await {
        // 1. When a new client connects, they send an empty message with their ClientTx
        //    (You'd use an Enum for this, but simplifying here for demonstration)
        if msg == "REGISTER" {
            // In a real system, you'd need the addr passed in the main loop
            // Here we assume the client's TX carries enough info or the command is robust
            // Skipping complex logic for now, but a proper solution requires Command Enum.
            clients.insert(sender_addr, client_tx);
            println!("Client registered: {}", sender_addr);
            continue; // Skip broadcast for registration
        }

        // 2. Broadcast/Print Logic (Simplification: assuming `client_tx` is always the sender)
        // let sender_addr = *clients
        //     .iter()
        //     .find(|(_, tx)| tx == &client_tx)
        //     .map(|(addr, _)| addr)
        //     .unwrap_or(&"UNKNOWN".parse().unwrap());

        let broadcast_msg = format!("{}: {}\n", sender_addr, msg.trim());

        // 🔥 Print to Server Terminal 🔥
        print!("{}", broadcast_msg);

        // Broadcast to all other clients
        for (addr, tx) in clients.iter() {
            if *addr != sender_addr {
                let _ = tx.send(broadcast_msg.clone());
            }
        }
    }
}
