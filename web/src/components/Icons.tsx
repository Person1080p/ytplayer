type P = { size?: number; className?: string };
const svg = (d: string, fill = false) =>
  function Icon({ size = 22, className }: P) {
    return (
      <svg
        className={className}
        width={size}
        height={size}
        viewBox="0 0 24 24"
        fill={fill ? "currentColor" : "none"}
        stroke={fill ? "none" : "currentColor"}
        strokeWidth={2}
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden
      >
        <path d={d} />
      </svg>
    );
  };

export const PlayIcon = svg("M7 4.5v15a1 1 0 0 0 1.5.86l12.5-7.5a1 1 0 0 0 0-1.72L8.5 3.64A1 1 0 0 0 7 4.5z", true);
export const PauseIcon = svg("M6 4h4v16H6zM14 4h4v16h-4z", true);
export const NextIcon = svg("M5 4.5v15l11-7.5zM18 4h2v16h-2z", true);
export const PrevIcon = svg("M19 4.5v15L8 12zM4 4h2v16H4z", true);
export const HomeIcon = svg("M3 10.5 12 3l9 7.5V20a1 1 0 0 1-1 1h-5v-6H9v6H4a1 1 0 0 1-1-1z");
export const SearchIcon = svg("M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM21 21l-4.35-4.35");
export const QueueIcon = svg("M3 6h13M3 12h13M3 18h8M17 15v6l5-3z");
export const LibraryIcon = svg("M4 4v16M9 4v16M14 4l6 16");
export const ListIcon = svg("M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01");
export const SettingsIcon = svg(
  "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z",
);
export const MoreIcon = svg("M12 6h.01M12 12h.01M12 18h.01");
export const ShuffleIcon = svg("M16 3h5v5M4 20 21 3M21 16v5h-5M15 15l6 6M4 4l5 5");
export const RadioIcon = svg("M4.9 19.1a10 10 0 0 1 0-14.2M7.8 16.2a6 6 0 0 1 0-8.4M16.2 7.8a6 6 0 0 1 0 8.4M19.1 4.9a10 10 0 0 1 0 14.2M12 12h.01");
const HEART = "M20.8 4.6a5.5 5.5 0 0 0-7.8 0L12 5.7l-1-1.1a5.5 5.5 0 0 0-7.8 7.8l1 1.1L12 21l7.8-7.5 1-1.1a5.5 5.5 0 0 0 0-7.8z";
export const HeartIcon = svg(HEART);
export const HeartFilledIcon = svg(HEART, true);
export const MatchIcon = svg("M7 7h11l-3-3M17 17H6l3 3");
export const PlusIcon = svg("M12 5v14M5 12h14");
export const TrashIcon = svg("M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6");
export const ChevronDown = svg("M6 9l6 6 6-6");
export const ChevronUp = svg("M6 15l6-6 6 6");
export const DragIcon = svg("M9 5h.01M9 12h.01M9 19h.01M15 5h.01M15 12h.01M15 19h.01");
export const VolumeIcon = svg("M11 5 6 9H2v6h4l5 4zM15.5 8.5a5 5 0 0 1 0 7M19 5a10 10 0 0 1 0 14");
export const VolumeLowIcon = svg("M11 5 6 9H2v6h4l5 4zM15.5 8.5a5 5 0 0 1 0 7");
export const VolumeMuteIcon = svg("M11 5 6 9H2v6h4l5 4zM22 9l-6 6M16 9l6 6");
export const LinkIcon = svg("M10 13a5 5 0 0 0 7.07 0l3-3a5 5 0 0 0-7.07-7.07l-1.5 1.5M14 11a5 5 0 0 0-7.07 0l-3 3a5 5 0 0 0 7.07 7.07l1.5-1.5");
export const CloseIcon = svg("M18 6 6 18M6 6l12 12");
export const BackIcon = svg("M15 18l-6-6 6-6");
export const PlayNextIcon = svg("M3 6h13M3 12h9M3 18h9M16 13l5 3-5 3z");
