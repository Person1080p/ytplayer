import { useEffect, useState } from "react";
import { Link, NavLink, Route, Routes, useLocation } from "react-router-dom";
import { api } from "./lib/api";
import { initPlayer } from "./lib/player";
import { loadProfile, useProfile } from "./lib/profile";
import { loadLibraryIds } from "./lib/library";
import ProfileToggle from "./components/ProfileToggle";
import { useTailnet } from "./lib/tailnet";
import { setToken as saveToken, useConnection, useTransportReady, watchConnection } from "./lib/transport";
import OAuthReturn from "./pages/OAuthReturn";
import { HomeIcon, LibraryIcon, ListIcon, QueueIcon, SearchIcon, SettingsIcon } from "./components/Icons";
import { NowPlaying, PlayerBar } from "./components/Player";
import { PlaylistPicker, PlexVersionSheet, Toast, TrackMenu } from "./components/Sheets";
import Home from "./pages/Home";
import Search from "./pages/Search";
import Collection from "./pages/Collection";
import Artist, { ArtistByName, BrowseGrid } from "./pages/Artist";
import QueuePage from "./pages/QueuePage";
import { PlaylistDetail, PlaylistsPage, SpotifyPlaylistPage } from "./pages/Playlists";
import Library from "./pages/Library";
import Settings from "./pages/Settings";

const NAV = [
  { to: "/", label: "Home", icon: HomeIcon, end: true },
  { to: "/search", label: "Search", icon: SearchIcon },
  { to: "/queue", label: "Queue", icon: QueueIcon },
  { to: "/playlists", label: "Playlists", icon: ListIcon },
  { to: "/library", label: "Library", icon: LibraryIcon },
  { to: "/settings", label: "Settings", icon: SettingsIcon },
];

function Login({ onDone }: { onDone: () => void }) {
  const [token, setToken] = useState("");
  const [err, setErr] = useState("");
  return (
    <div className="login">
      <img src="/icon.svg" width={72} height={72} alt="" />
      <h1>YTP Music</h1>
      <form
        onSubmit={async (e) => {
          e.preventDefault();
          try {
            await api.login(token);
            saveToken(token); // also sent as a header (cookies don't travel over the in-page tailnet)
            onDone();
          } catch {
            setErr("Wrong access token");
          }
        }}
      >
        <input className="input" type="password" placeholder="Access token" value={token} onChange={(e) => setToken(e.target.value)} autoFocus />
        <button className="btn" type="submit">
          Unlock
        </button>
        {err && <p className="error">{err}</p>}
      </form>
    </div>
  );
}

/** Slim, non-blocking notice shown while there's no working connection to the server. */
function ConnectionBanner() {
  const ready = useTransportReady();
  const conn = useConnection((s) => s.conn);
  const directAvailable = useConnection((s) => s.directAvailable);
  const phase = useTailnet((s) => s.phase);
  const { pathname } = useLocation();
  if (ready || pathname === "/settings") return null;
  if (!conn && directAvailable === null) return null; // still probing

  let text = "Your ytp server isn't reachable from this address.";
  if (conn?.mode === "tailnet") {
    if (phase === "loading" || phase === "starting") text = "Connecting to your tailnet…";
    else if (phase === "needs-login") text = "Tailscale sign-in needed.";
    else if (phase === "needs-approval") text = "Waiting for this device to be approved in Tailscale.";
    else if (phase === "error") text = "Couldn't start Tailscale.";
  }
  const connecting = conn?.mode === "tailnet" && (phase === "loading" || phase === "starting");
  return (
    <div className="conn-banner">
      <span>{text}</span>
      {!connecting && (
        <Link to="/settings#connection" className="btn small">
          Set up connection
        </Link>
      )}
    </div>
  );
}

export default function App() {
  const ready = useTransportReady();
  const conn = useConnection((s) => s.conn);
  const [auth, setAuth] = useState<"unknown" | "ok" | "needed">("unknown");

  // Direct if the page's own address reaches the server, otherwise the Tailscale fallback.
  // Runs in the background and keeps re-checking; the UI never waits on it.
  useEffect(() => {
    watchConnection();
  }, []);

  // Once connected, check the (optional) access token.
  useEffect(() => {
    if (!ready) return;
    api
      .authCheck()
      .then((r) => setAuth(r.authenticated ? "ok" : "needed"))
      .catch(() => setAuth("ok"));
    const onUnauth = () => setAuth("needed");
    window.addEventListener("ytp:unauthorized", onUnauth);
    return () => window.removeEventListener("ytp:unauthorized", onUnauth);
  }, [ready, conn]);

  const online = ready && auth === "ok";
  const profile = useProfile((s) => s.active);
  useEffect(() => {
    if (online) {
      initPlayer();
      loadProfile();
      loadLibraryIds();
    }
  }, [online]);

  if (auth === "needed") return <Login onDone={() => setAuth("ok")} />;

  return (
    <div className="app">
      <nav className="sidebar">
        <div className="brand">
          <img src="/icon.svg" width={28} height={28} alt="" /> YTP
        </div>
        {NAV.map(({ to, label, icon: Icon, end }) => (
          <NavLink key={to} to={to} end={end} className="nav-link">
            <Icon size={20} /> <span>{label}</span>
          </NavLink>
        ))}
        {online && (
          <div className="sidebar-foot">
            <ProfileToggle />
          </div>
        )}
      </nav>
      {/* Remount pages (refetch everything) when the connection or listener profile changes. */}
      <main className="content" key={online ? `online-${conn?.mode}-${profile}` : "offline"}>
        <ConnectionBanner />
        <Routes>
          <Route path="/" element={<Home />} />
          <Route path="/search" element={<Search />} />
          <Route path="/collection/:id" element={<Collection />} />
          <Route path="/artist" element={<ArtistByName />} />
          <Route path="/artist/:id" element={<Artist />} />
          <Route path="/browse/:id" element={<BrowseGrid />} />
          <Route path="/queue" element={<QueuePage />} />
          <Route path="/playlists" element={<PlaylistsPage />} />
          <Route path="/playlists/:id" element={<PlaylistDetail />} />
          <Route path="/spotify/:id" element={<SpotifyPlaylistPage />} />
          <Route path="/library" element={<Library />} />
          <Route path="/settings" element={<Settings />} />
          <Route path="/oauth/:provider" element={<OAuthReturn />} />
        </Routes>
      </main>
      <PlayerBar />
      <nav className="bottom-nav">
        {NAV.filter((n) => n.to !== "/playlists").map(({ to, label, icon: Icon, end }) => (
          <NavLink key={to} to={to} end={end} className="nav-link">
            <Icon size={22} />
            <span>{label}</span>
          </NavLink>
        ))}
      </nav>
      <NowPlaying />
      <TrackMenu />
      <PlaylistPicker />
      <PlexVersionSheet />
      <Toast />
    </div>
  );
}
