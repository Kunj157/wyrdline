/**
 * The static weave: what the hero shows before the 3D Loom loads, when
 * WebGL is unavailable, and under reduced motion.
 */
const calls = [
  { x: 70, fx: [120, 150, 175] },
  { x: 210, fx: [250, 275, 300, 330] },
  { x: 360, fx: [395, 420] },
  { x: 470, fx: [505, 530, 560, 585, 610] },
  { x: 640, fx: [680, 705] },
  { x: 750, fx: [785, 810, 840] },
];

export function WeavePoster() {
  return (
    <svg
      viewBox="0 0 900 360"
      role="img"
      aria-label="Threads from tool calls on an upper rail weaving down to the kernel effects they caused"
      className="absolute inset-0 size-full"
      preserveAspectRatio="xMidYMid meet"
    >
      <defs>
        <linearGradient id="spine" x1="0" x2="1">
          <stop offset="0" stopColor="var(--color-vivid-sky)" />
          <stop offset="1" stopColor="var(--color-vivid-violet)" />
        </linearGradient>
      </defs>
      <path
        d="M20 180 C 300 170, 600 190, 880 180"
        stroke="url(#spine)"
        strokeWidth="3"
        fill="none"
      />
      {calls.map((c, i) =>
        c.fx.map((x) => (
          <path
            key={`${c.x}-${x}`}
            d={`M${c.x} 70 C ${c.x + 20} 160, ${x - 40} 210, ${x} 290`}
            stroke={i % 4 === 1 ? "var(--color-vivid-violet)" : "var(--color-accent)"}
            strokeWidth="2"
            strokeOpacity="0.75"
            fill="none"
          />
        )),
      )}
      {[130, 280, 430, 580, 730].map((x) => (
        <circle
          key={x}
          cx={x}
          cy="180"
          r="11"
          fill="none"
          stroke="var(--color-vivid-violet)"
          strokeWidth="4"
        />
      ))}
      {calls.map((c) => (
        <circle key={c.x} cx={c.x} cy="70" r="11" fill="var(--color-vivid-verdigris)" />
      ))}
      {calls.flatMap((c) =>
        c.fx.map((x, j) => (
          <circle
            key={x}
            cx={x}
            cy="290"
            r="8"
            fill={
              x === 560
                ? "var(--color-vivid-vermilion)"
                : j === 3
                  ? "var(--color-vivid-ochre)"
                  : "var(--color-vivid-bone)"
            }
            stroke="var(--color-effect)"
            strokeOpacity="0.35"
          />
        )),
      )}
    </svg>
  );
}
