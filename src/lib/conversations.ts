// État des conversations, tenu HORS de la page qui les affiche.
//
// Il vivait dans le composant `Conversations` : quitter la page le démontait,
// et avec lui le fil, le texte en cours d'écriture et la progression. La
// réflexion, elle, continuait côté Rust — mais sa réponse arrivait dans un
// composant mort. Pour l'utilisateur, c'était impossible à distinguer d'une
// interruption. Pire : changer de conversation sans quitter la page faisait
// atterrir la réponse en vol dans le fil affiché, donc dans la MAUVAISE
// conversation.
//
// Ici, chaque conversation possède son état, indexé par son identifiant. La
// page n'est plus qu'une vue : on peut la démonter sans rien perdre.
import { createStore, produce } from "solid-js/store";
import { createSignal } from "solid-js";
import {
  ipc,
  on,
  type AccountChoice,
  type AgentProgress,
  type Answer,
  type Retrieved,
  type ScreenContext,
} from "./ipc";
import { page, refreshPending, setSessionsVersion } from "./state";

export interface ConvoMsg {
  role: "user" | "assistant" | "note";
  content: string;
  sources?: Retrieved[];
  degraded?: boolean;
  choices?: AccountChoice[];
}

export interface ConvoState {
  messages: ConvoMsg[];
  /// Une réflexion est-elle en cours pour CETTE conversation ?
  running: boolean;
  /// Réponse en cours d'écriture, diffusée au fil de l'eau.
  streaming: string;
  progress: AgentProgress[];
  /// Le fil a-t-il déjà été chargé depuis la base ?
  loaded: boolean;
  /// Un tour resté sans réponse : l'application a été quittée pendant la
  /// réflexion. On le dit, et on propose de relancer.
  interrupted: boolean;
  /// Syn a fini de répondre pendant que l'utilisateur regardait ailleurs. La
  /// réponse attend d'être lue — un anneau qui disparaît sans rien laisser
  /// derrière lui ne se remarque pas.
  unread: boolean;
}

const EMPTY: ConvoState = {
  messages: [],
  running: false,
  streaming: "",
  progress: [],
  loaded: false,
  interrupted: false,
  unread: false,
};

const [conversations, setConversations] = createStore<Record<string, ConvoState>>({});

/// La conversation affichée. Globale elle aussi : revenir sur la page retrouve
/// le fil qu'on y avait laissé.
export const [activeSession, setActiveSession] = createSignal<string | null>(null);

export function conversation(id: string | null): ConvoState {
  return (id && conversations[id]) || EMPTY;
}

function ensure(id: string) {
  if (!conversations[id]) setConversations(id, { ...EMPTY });
}

/// Les conversations sur lesquelles Syn travaille en ce moment.
export function runningSessions(): string[] {
  return Object.keys(conversations).filter((id) => conversations[id]?.running);
}

export function isRunning(id: string | null): boolean {
  return !!id && !!conversations[id]?.running;
}

/// Les conversations dont la réponse n'a pas encore été lue.
export function unreadSessions(): string[] {
  return Object.keys(conversations).filter((id) => conversations[id]?.unread);
}

export function isUnread(id: string | null): boolean {
  return !!id && !!conversations[id]?.unread;
}

/// L'utilisateur regarde-t-il cette conversation en ce moment ? Être sur une
/// autre page compte comme regarder ailleurs.
function isWatching(id: string): boolean {
  return page() === "conversations" && activeSession() === id;
}

/// La réponse a été vue : la pastille s'efface.
export function markRead(id: string): void {
  if (conversations[id]?.unread) setConversations(id, { unread: false });
}

/// Un tour utilisateur sans réponse termine le fil : la réflexion a été
/// interrompue par la fermeture de l'application.
function endsOnUnansweredTurn(messages: ConvoMsg[]): boolean {
  const last = messages[messages.length - 1];
  return !!last && last.role === "user";
}

export async function loadConversation(id: string, force = false): Promise<void> {
  ensure(id);
  if (conversations[id].loaded && !force) return;
  const turns = await ipc.getConversation(id).catch(() => null);
  if (!turns) return;
  const messages: ConvoMsg[] = turns.map((turn: any) => ({
    role: turn.role,
    content: turn.content,
  }));
  // Les choix proposés (comptes d'envoi) ne sont pas persistés : ils vivent
  // avec la réponse. Un rechargement du fil ne doit pas escamoter les boutons
  // que l'utilisateur n'a pas encore cliqués.
  const previous = conversations[id].messages;
  const lastKnown = previous[previous.length - 1];
  const lastLoaded = messages[messages.length - 1];
  if (
    lastKnown?.choices?.length &&
    lastLoaded?.role === "assistant" &&
    lastLoaded.content === lastKnown.content
  ) {
    lastLoaded.choices = lastKnown.choices;
  }
  setConversations(id, {
    messages,
    loaded: true,
    // Une réponse en cours n'est pas une interruption.
    interrupted: !conversations[id].running && endsOnUnansweredTurn(messages),
  });
}

export async function openConversation(id: string): Promise<void> {
  setActiveSession(id);
  markRead(id);
  await loadConversation(id);
}

export function startNewConversation(): void {
  setActiveSession(null);
}

/// Envoie une demande et RANGE la réponse dans sa conversation d'origine, quelle
/// que soit celle affichée quand elle arrive.
export async function sendMessage(
  sessionId: string | null,
  text: string,
  screenContext?: ScreenContext | null,
): Promise<void> {
  const id = sessionId ?? crypto.randomUUID();
  ensure(id);
  if (conversations[id].running) return;
  setActiveSession((current) => current ?? id);
  setConversations(
    id,
    produce((state) => {
      state.messages.push({ role: "user", content: text });
      state.running = true;
      state.streaming = "";
      state.progress = [];
      state.interrupted = false;
      state.loaded = true;
    }),
  );
  // La conversation apparaît dans la liste dès que le moteur l'a créée : sans
  // ce rafraîchissement, l'anneau de travail n'aurait aucune ligne où s'afficher.
  setTimeout(() => setSessionsVersion((v) => v + 1), 400);
  try {
    const answer: Answer = await ipc.query(id, text, screenContext);
    setConversations(
      answer.session_id === id ? id : answer.session_id,
      produce((state) => {
        state.messages.push({
          role: "assistant",
          content: answer.text,
          sources: answer.sources,
          degraded: answer.degraded,
          choices: answer.choices,
        });
        state.streaming = "";
      }),
    );
    void refreshPending();
  } catch (e: any) {
    setConversations(
      id,
      produce((state) => {
        state.messages.push({
          role: "assistant",
          content: `⚠ ${e?.message ?? e}`,
          degraded: true,
        });
        state.streaming = "";
      }),
    );
  } finally {
    setConversations(id, { running: false, unread: !isWatching(id) });
    setSessionsVersion((v) => v + 1);
  }
}

/// Relance le dernier message resté sans réponse.
export async function retryLastTurn(id: string): Promise<void> {
  const messages = conversation(id).messages;
  const last = messages[messages.length - 1];
  if (!last || last.role !== "user") return;
  setConversations(
    id,
    produce((state) => {
      state.messages.pop();
      state.interrupted = false;
    }),
  );
  await sendMessage(id, last.content);
}

export function forgetConversation(id: string): void {
  setConversations(id, undefined!);
  if (activeSession() === id) setActiveSession(null);
}

/// Le compte d'envoi choisi d'un clic : Syn enchaîne, puis le fil est rechargé
/// depuis la base (l'accusé « Vous avez choisi… » y est déjà écrit).
export async function chooseMailAccount(id: string, via: string): Promise<void> {
  ensure(id);
  setConversations(id, { running: true });
  try {
    await ipc.chooseMailAccount(id, via);
    await loadConversation(id, true);
    await refreshPending();
  } catch (e: any) {
    setConversations(
      id,
      produce((state) => {
        state.messages.push({
          role: "assistant",
          content: `⚠ ${e?.message ?? e}`,
          degraded: true,
        });
      }),
    );
  } finally {
    setConversations(id, { running: false, unread: !isWatching(id) });
  }
}

/// Les événements du moteur sont écoutés UNE fois, au niveau de l'application.
/// Attachés à la page, ils disparaissaient avec elle : une conversation laissée
/// en arrière-plan perdait sa progression et son texte en cours d'écriture.
let wired = false;
export async function wireConversationEvents(): Promise<void> {
  if (wired) return;
  wired = true;

  await on("agent_progress", (raw) => {
    const event = (raw?.payload ?? raw) as AgentProgress;
    if (!event?.session_id || !conversations[event.session_id]) return;
    setConversations(
      event.session_id,
      produce((state) => {
        state.progress = [...state.progress, event].slice(-20);
      }),
    );
  });

  await on("answer_delta", (raw) => {
    const event = (raw?.payload ?? raw) as { session_id: string; delta: string };
    if (!event?.delta || !conversations[event.session_id]) return;
    setConversations(
      event.session_id,
      produce((state) => {
        state.streaming += event.delta;
      }),
    );
  });

  await on("semantic_results", (raw) => {
    const event = (raw?.payload ?? raw) as { session_id: string; results: Retrieved[] };
    if (!event?.results?.length || !conversations[event.session_id]) return;
    setConversations(
      event.session_id,
      produce((state) => {
        const index = state.messages.findLastIndex((message) => message.role === "assistant");
        if (index < 0) return;
        const existing = state.messages[index].sources ?? [];
        const seen = new Set(existing.map((source) => source.item_id));
        state.messages[index].sources = [
          ...existing,
          ...event.results.filter((source) => !seen.has(source.item_id)),
        ];
      }),
    );
  });

  // Une action confirmée écrit dans le fil côté moteur (compte rendu, accusés) :
  // on recharge la conversation concernée, même si elle n'est pas affichée.
  await on("action_resolved", () => {
    for (const id of Object.keys(conversations)) {
      void loadConversation(id, true);
      if (!isWatching(id)) setConversations(id, { unread: true });
    }
  });
}
