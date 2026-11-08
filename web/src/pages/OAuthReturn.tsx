import { useEffect, useRef, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { api } from "../lib/api";
import { useStore } from "../lib/store";

/**
 * OAuth providers redirect back to a page route (not the server), so logins work the same
 * whether this device reaches the server directly or through the in-page tailnet.
 */
export default function OAuthReturn() {
  const { provider } = useParams();
  const [params] = useSearchParams();
  const nav = useNavigate();
  const toast = useStore((s) => s.toastMsg);
  const [error, setError] = useState<string | null>(null);
  const done = useRef(false);

  useEffect(() => {
    if (done.current) return;
    done.current = true;
    (async () => {
      try {
        if (provider === "lastfm") {
          const token = params.get("token");
          if (!token) throw new Error("Last.fm didn't return a token");
          const r = await api.lastfmSession(token);
          toast(`Last.fm connected as ${r.username}`);
          nav("/settings", { replace: true });
        } else {
          throw new Error("Unknown provider");
        }
      } catch (e) {
        setError((e as Error).message);
      }
    })();
  }, [provider, params, nav, toast]);

  return (
    <div className="page">
      {error ? (
        <>
          <p className="error">Login failed: {error}</p>
          <button className="btn ghost" onClick={() => nav("/settings")}>
            Back to settings
          </button>
        </>
      ) : (
        <div className="spinner" />
      )}
    </div>
  );
}
