import React, { useCallback, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import {
  Check,
  CircleAlert,
  Cloud,
  Copy,
  KeyRound,
  LoaderCircle,
  LockKeyhole,
  LogOut,
  RefreshCw,
  ShieldCheck,
  UserRound,
  X,
} from "lucide-react";
import "./desktop.css";

type OAuthPhase = "signed-out" | "waiting" | "exchanging" | "signed-in" | "failed";

interface OAuthStatus {
  phase: OAuthPhase;
  accountName?: string;
  accountId?: string;
  message?: string;
}

interface OAuthClientConfiguration {
  clientId?: string;
  buildProvided: boolean;
  redirectUri: string;
  pkceMethod: "S256";
  clientSecretEmbedded: false;
}

const SOURCE_CLIENT_ID_KEY = "neuman.roblox.public-client-id";
const defaultConfiguration: OAuthClientConfiguration = {
  buildProvided: false,
  redirectUri: "http://localhost:43891/oauth/callback",
  pkceMethod: "S256",
  clientSecretEmbedded: false,
};

async function backend<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!("__TAURI_INTERNALS__" in window)) {
    if (command === "oauth_status") return { phase: "signed-out" } as T;
    if (command === "oauth_client_configuration") return defaultConfiguration as T;
    if (command === "start_roblox_oauth") return { expiresInSeconds: 300 } as T;
    if (command === "cancel_roblox_oauth" || command === "logout_roblox") return undefined as T;
    throw new Error("This operation requires the NeuMan desktop runtime.");
  }
  return invoke<T>(command, args);
}

function Brand() {
  return (
    <div className="brand" aria-label="NeuMan">
      <span className="brand-mark">N</span>
      <span>NeuMan</span>
    </div>
  );
}

function SecurityNote() {
  return (
    <div className="security-note">
      <ShieldCheck size={17} aria-hidden="true" />
      <span>Your password stays with Roblox. NeuMan stores the resulting session in Windows Credential Manager.</span>
    </div>
  );
}

function RobloxGlyph({ connected = false }: { connected?: boolean }) {
  return (
    <div className={connected ? "roblox-glyph connected" : "roblox-glyph"} aria-hidden="true">
      {connected ? <Check size={34} strokeWidth={2.5} /> : <span />}
    </div>
  );
}

function DeveloperSetup({
  clientId,
  setClientId,
  redirectUri,
}: {
  clientId: string;
  setClientId: (value: string) => void;
  redirectUri: string;
}) {
  const [copied, setCopied] = useState(false);

  const copyRedirect = async () => {
    await navigator.clipboard.writeText(redirectUri);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1600);
  };

  return (
    <div className="developer-setup standalone-setup">
      <div className="developer-body">
        <label>
          <span>Public client ID</span>
          <input
            value={clientId}
            onChange={(event) => setClientId(event.target.value.replace(/\s/g, ""))}
            placeholder="Enter your client ID"
            autoComplete="off"
            spellCheck={false}
          />
        </label>
        <div className="redirect-row">
          <div>
            <span>Registered callback</span>
            <code>{redirectUri}</code>
          </div>
          <button className="copy-button" onClick={() => void copyRedirect()} aria-label="Copy callback URL">
            {copied ? <Check size={15} /> : <Copy size={15} />}
          </button>
        </div>
        <p className="developer-footnote">The client ID is public and may be saved on this device. Never enter a client secret.</p>
      </div>
    </div>
  );
}

function App() {
  const [booting, setBooting] = useState(true);
  const [oauth, setOauth] = useState<OAuthStatus>({ phase: "signed-out" });
  const [configuration, setConfiguration] = useState(defaultConfiguration);
  const [clientId, setClientId] = useState(() => window.localStorage.getItem(SOURCE_CLIENT_ID_KEY) ?? "");
  const [developerSetupOpen, setDeveloperSetupOpen] = useState(
    () => !/^[A-Za-z0-9]{1,128}$/.test(window.localStorage.getItem(SOURCE_CLIENT_ID_KEY) ?? ""),
  );
  const [busy, setBusy] = useState(false);
  const [localError, setLocalError] = useState<string>();
  const [confirmDisconnect, setConfirmDisconnect] = useState(false);

  const refreshStatus = useCallback(async () => {
    try {
      setOauth(await backend<OAuthStatus>("oauth_status"));
    } catch (error) {
      setOauth({
        phase: "failed",
        message: error instanceof Error ? error.message : String(error),
      });
    }
  }, []);

  useEffect(() => {
    let active = true;
    void Promise.all([
      backend<OAuthStatus>("oauth_status"),
      backend<OAuthClientConfiguration>("oauth_client_configuration"),
    ])
      .then(([status, clientConfiguration]) => {
        if (!active) return;
        setOauth(status);
        setConfiguration(clientConfiguration);
        if (clientConfiguration.clientId) {
          setClientId(clientConfiguration.clientId);
          setDeveloperSetupOpen(false);
        }
      })
      .catch((error) => {
        if (!active) return;
        setOauth({ phase: "failed", message: error instanceof Error ? error.message : String(error) });
      })
      .finally(() => {
        if (active) setBooting(false);
      });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (!configuration.buildProvided && clientId) {
      window.localStorage.setItem(SOURCE_CLIENT_ID_KEY, clientId);
    }
  }, [clientId, configuration.buildProvided]);

  useEffect(() => {
    if (oauth.phase !== "waiting" && oauth.phase !== "exchanging") return undefined;
    const timer = window.setInterval(() => void refreshStatus(), 750);
    return () => window.clearInterval(timer);
  }, [oauth.phase, refreshStatus]);

  const effectiveClientId = configuration.clientId ?? clientId.trim();
  const validClientId = /^[A-Za-z0-9]{1,128}$/.test(effectiveClientId);

  const connect = async () => {
    setLocalError(undefined);
    if (!validClientId) {
      setLocalError("Enter the public client ID from your Roblox OAuth application.");
      return;
    }
    setBusy(true);
    try {
      await backend("start_roblox_oauth", { request: { clientId: effectiveClientId } });
      setOauth({ phase: "waiting", message: "Waiting for authorization in your browser." });
    } catch (error) {
      setOauth({ phase: "failed", message: error instanceof Error ? error.message : String(error) });
    } finally {
      setBusy(false);
    }
  };

  const cancel = async () => {
    setBusy(true);
    try {
      await backend("cancel_roblox_oauth");
      setOauth({ phase: "signed-out" });
    } catch (error) {
      setLocalError(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  const disconnect = async () => {
    setBusy(true);
    setLocalError(undefined);
    try {
      await backend("logout_roblox");
      setOauth({ phase: "signed-out" });
      setConfirmDisconnect(false);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (message.startsWith("Signed out locally")) {
        setOauth({ phase: "signed-out" });
        setConfirmDisconnect(false);
      }
      setLocalError(message);
    } finally {
      setBusy(false);
    }
  };

  const content = (() => {
    if (booting) {
      return (
        <div className="state-content compact-state" aria-live="polite">
          <LoaderCircle className="spinner" size={28} />
          <h1>Preparing a secure connection…</h1>
        </div>
      );
    }

    if (oauth.phase === "signed-out" && !configuration.buildProvided && developerSetupOpen) {
      return (
        <div className="state-content developer-state">
          <div className="setup-glyph"><KeyRound size={27} /></div>
          <p className="eyebrow">Development build</p>
          <h1>Set up Roblox OAuth</h1>
          <p className="lead">Create a public Roblox OAuth app, then add its client ID here. This is a one-time setup for source builds.</p>
          <DeveloperSetup clientId={clientId} setClientId={setClientId} redirectUri={configuration.redirectUri} />
          <button className="button primary setup-continue" onClick={() => setDeveloperSetupOpen(false)} disabled={!validClientId}>
            Continue <Check size={17} />
          </button>
        </div>
      );
    }

    if (oauth.phase === "waiting") {
      return (
        <div className="state-content" aria-live="polite">
          <div className="progress-orbit"><Cloud size={28} /><span /></div>
          <p className="eyebrow">Browser opened</p>
          <h1>Finish connecting on Roblox</h1>
          <p className="lead">Choose your account and approve access. This window will update automatically.</p>
          <div className="waiting-row"><LoaderCircle className="spinner" size={17} /> Waiting for Roblox</div>
          <button className="text-button" onClick={() => void cancel()} disabled={busy}>Cancel</button>
        </div>
      );
    }

    if (oauth.phase === "exchanging") {
      return (
        <div className="state-content" aria-live="polite">
          <div className="progress-orbit verifying"><LockKeyhole size={27} /><span /></div>
          <p className="eyebrow">Authorization received</p>
          <h1>Securing your connection</h1>
          <p className="lead">NeuMan is validating Roblox’s response and protecting your session on this device.</p>
          <div className="waiting-row"><LoaderCircle className="spinner" size={17} /> Verifying identity</div>
        </div>
      );
    }

    if (oauth.phase === "signed-in") {
      return (
        <div className="state-content connected-state" aria-live="polite">
          <RobloxGlyph connected />
          <p className="eyebrow success">Connection complete</p>
          <h1>You’re connected to Roblox</h1>
          <p className="lead">NeuMan can now identify your account without ever handling your Roblox password.</p>
          <div className="account-card">
            <span className="account-avatar"><UserRound size={23} /></span>
            <div><strong>{oauth.accountName ?? "Roblox account"}</strong><span>User ID {oauth.accountId ?? "verified"}</span></div>
            <span className="connected-pill"><span /> Connected</span>
          </div>
          <div className="vault-confirmation"><LockKeyhole size={16} /> Session protected by Windows</div>
          {!confirmDisconnect ? (
            <button className="text-button danger-text" onClick={() => setConfirmDisconnect(true)}>Disconnect account</button>
          ) : (
            <div className="disconnect-confirm">
              <span>Disconnect this Roblox account?</span>
              <div>
                <button className="button secondary" onClick={() => setConfirmDisconnect(false)} disabled={busy}>Keep connected</button>
                <button className="button danger" onClick={() => void disconnect()} disabled={busy}>
                  <LogOut size={16} /> Disconnect
                </button>
              </div>
            </div>
          )}
        </div>
      );
    }

    if (oauth.phase === "failed") {
      return (
        <div className="state-content" aria-live="assertive">
          <div className="error-glyph"><X size={30} /></div>
          <p className="eyebrow error">Connection interrupted</p>
          <h1>We couldn’t connect to Roblox</h1>
          <p className="lead">Nothing was saved. Check the detail below, then try again.</p>
          <div className="error-message"><CircleAlert size={17} />{oauth.message ?? "Roblox authorization did not complete."}</div>
          <button className="button primary" onClick={() => { setOauth({ phase: "signed-out" }); setLocalError(undefined); }}>
            <RefreshCw size={17} /> Try again
          </button>
        </div>
      );
    }

    return (
      <div className="state-content">
        <RobloxGlyph />
        <p className="eyebrow">Step one</p>
        <h1>Connect your Roblox account</h1>
        <p className="lead">NeuMan will open Roblox in your browser. Sign in there, approve access, and come straight back.</p>
        <button className="button primary connect-button" onClick={() => void connect()} disabled={busy || !validClientId}>
          {busy ? <LoaderCircle className="spinner" size={19} /> : <Cloud size={19} />}
          {busy ? "Opening Roblox…" : "Connect Roblox"}
        </button>
        {localError && <div className="inline-error"><CircleAlert size={16} />{localError}</div>}
        {!configuration.buildProvided && <button className="text-button setup-link" onClick={() => setDeveloperSetupOpen(true)}>Change OAuth app</button>}
        <SecurityNote />
      </div>
    );
  })();

  return (
    <div className="app-shell">
      <header><Brand /><span className="local-badge"><span /> Local desktop app</span></header>
      <main>
        <section className="connection-card">{content}</section>
      </main>
      <footer><LockKeyhole size={14} /> OAuth 2.0 · PKCE · OS-protected session</footer>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
