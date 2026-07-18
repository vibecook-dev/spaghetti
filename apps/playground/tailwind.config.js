/** @type {import('tailwindcss').Config} */
export default {
  darkMode: 'class',
  content: ['./src/renderer/**/*.{html,ts,tsx}', '../../packages/sdk/src/react/**/*.{ts,tsx}'],
  theme: {
    extend: {
      colors: {
        border: 'var(--border)',
        background: 'var(--background)',
        foreground: 'var(--foreground)',
        card: {
          DEFAULT: 'var(--card)',
          foreground: 'var(--card-foreground)',
        },
        primary: {
          DEFAULT: 'var(--primary)',
          foreground: 'var(--primary-foreground)',
        },
        secondary: {
          DEFAULT: 'var(--secondary)',
          foreground: 'var(--secondary-foreground)',
        },
        muted: {
          DEFAULT: 'var(--muted)',
          foreground: 'var(--muted-foreground)',
        },
        accent: {
          DEFAULT: 'var(--accent)',
          foreground: 'var(--accent-foreground)',
        },
        destructive: {
          DEFAULT: 'var(--destructive)',
        },
        // Archive palette (spaghetti-ui-design)
        ink: {
          DEFAULT: 'var(--archive-ink)',
          muted: 'var(--archive-ink-muted)',
          faint: 'var(--archive-ink-faint)',
          line: 'var(--archive-ink-line)',
        },
        paper: {
          DEFAULT: 'var(--archive-paper)',
          deep: 'var(--archive-paper-deep)',
          bright: 'var(--archive-paper-bright)',
        },
        chrome: 'var(--archive-chrome)',
        sanguine: 'var(--archive-sanguine)',
        verdigris: 'var(--archive-verdigris)',
        indigo: 'var(--archive-indigo)',
        faded: 'var(--archive-faded)',
        ochre: 'var(--archive-ochre)',
      },
      fontFamily: {
        serif: ['"EB Garamond"', 'Georgia', 'Times New Roman', 'serif'],
        mono: ['ui-monospace', 'SF Mono', 'Menlo', 'Consolas', 'monospace'],
      },
      borderRadius: {
        lg: '0',
        md: '0',
        sm: '0',
        none: '0',
      },
    },
  },
  plugins: [],
  safelist: [
    {
      pattern:
        /^(bg|text|border|border-l)-(green|yellow|cyan|blue|red|purple|indigo|amber|gray|white|orange)(-(50|100|200|300|400|500|600|700))?(\/\d+)?$/,
    },
    'bg-green-500/[0.04]',
    'bg-blue-500/[0.04]',
    'bg-red-500/[0.04]',
    'bg-purple-500/[0.02]',
    'bg-purple-500/[0.03]',
    'bg-amber-500/[0.04]',
    'bg-cyan-500/[0.04]',
    'bg-indigo-500/[0.02]',
    'bg-indigo-500/10',
    'bg-white/[0.02]',
    'bg-white/[0.03]',
    'bg-white/5',
    'bg-white/10',
    'border-purple-500/10',
    'border-blue-500/10',
    'border-indigo-500/20',
    'border-indigo-500/30',
    'text-blue-300/60',
    'text-indigo-300/60',
    'hover:bg-indigo-500/10',
    'hover:text-indigo-300',
    'hover:text-white/60',
    'hover:bg-white/5',
    'hover:bg-white/10',
  ],
};
