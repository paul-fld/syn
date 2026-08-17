use serde::Serialize;
use tokio::sync::broadcast;

/// Event bus interne (doc maître §6). Relayé vers le frontend par ipc::forward_bus.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", content = "payload", rename_all = "snake_case")]
pub enum BusEvent {
    ItemIngested {
        item_id: String,
        source: String,
        title: String,
    },
    IngestionStatus {
        state: String,
        current: Option<String>,
        done: u64,
        total: u64,
    },
    FilesError {
        path: String,
        reason: String,
    },
    /// Les modèles sont chargés : l'écran de démarrage peut s'effacer.
    RuntimeReady,
    /// Fragment de réponse produit au fil de la génération. Sans lui, Syn
    /// attend d'avoir écrit sa réponse ENTIÈRE avant d'afficher le premier mot :
    /// le temps d'attente est le même, mais il est vécu comme un blocage.
    AnswerDelta {
        session_id: String,
        delta: String,
    },
    SemanticResults {
        session_id: String,
        results: Vec<crate::retrieval::Retrieved>,
    },
    SyncProgress {
        connector: String,
        pct: f32,
        message: Option<String>,
    },
    BriefReady,
    ProactiveAlert {
        id: String,
        kind: String,
        reason: String,
        body: String,
        priority: String,
    },
    ActionAwaitingConfirmation {
        action_id: String,
        tool: String,
        preview: String,
        risk_class: String,
    },
    ActionResolved {
        action_id: String,
        status: String,
    },
    SystemAlert {
        reason: String,
        body: String,
    },
    ModelPullProgress {
        model: String,
        pct: f32,
        status: String,
    },
    AgentProgress {
        session_id: String,
        stage: String,
        title: String,
        detail: Option<String>,
        current: u32,
        total: u32,
        status: String,
    },
    VoiceProfileChanged,
    WakeFromSleep,
}

#[derive(Clone)]
pub struct Bus {
    tx: broadcast::Sender<BusEvent>,
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Bus { tx }
    }
    pub fn emit(&self, ev: BusEvent) {
        let _ = self.tx.send(ev);
    }
    pub fn subscribe(&self) -> broadcast::Receiver<BusEvent> {
        self.tx.subscribe()
    }
}
