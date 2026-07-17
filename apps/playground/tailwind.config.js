/** @type {import('tailwindcss').Config} */
export default {
  darkMode: 'class',
  content: [
    './src/renderer/**/*.{html,ts,tsx}',
    // MessageEntry + timeline chat UI
    '../../packages/sdk/src/react/**/*.{ts,tsx}',
  ],
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
      },
      borderRadius: {
        lg: 'var(--radius)',
        md: 'calc(var(--radius) - 2px)',
        sm: 'calc(var(--radius) - 4px)',
      },
    },
  },
  plugins: [],
  // Badge builds `bg-${color}` dynamically; safelist common MessageEntry accents.
  safelist: [
    {
      pattern:
        /^(bg|text|border|border-l)-(green|yellow|cyan|blue|red|purple|indigo|amber|gray|white)(-(50|100|200|300|400|500|600|700))?(\/\d+)?$/,
    },
    // Arbitrary opacity backgrounds used by MessageEntry
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
