/// Alert notifier for sending messages via Telegram or webhooks.
pub struct AlertNotifier {
    webhook_url: String,
    client: reqwest::Client,
}

impl AlertNotifier {
    pub fn new(webhook_url: String) -> Self {
        Self {
            webhook_url,
            client: reqwest::Client::new(),
        }
    }

    /// Send an alert message.
    pub async fn send(&self, message: &str) -> anyhow::Result<()> {
        if self.webhook_url.is_empty() {
            tracing::debug!("No webhook configured, skipping alert");
            return Ok(());
        }

        self.client
            .post(&self.webhook_url)
            .json(&serde_json::json!({ "text": message }))
            .send()
            .await?;

        tracing::info!("Alert sent: {}", message);
        Ok(())
    }

    /// Send a circuit breaker alert.
    pub async fn alert_circuit_breaker(&self) -> anyhow::Result<()> {
        self.send("🚨 PolyAlpha Circuit Breaker TRIGGERED — all trading halted")
            .await
    }
}
