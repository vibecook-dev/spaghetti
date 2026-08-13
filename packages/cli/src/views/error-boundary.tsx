/**
 * ErrorBoundary — catches errors thrown while rendering a view.
 *
 * Views load async SDK queries into React state. A render-time failure in that
 * state or in presentation code would otherwise tear down the whole Ink tree
 * and dump a raw React stack over the alt-screen canvas. This boundary catches
 * it and shows a compact panel, letting the user step back or quit.
 *
 * It clears its caught error whenever `resetKey` changes, so navigating away
 * from a crashed view (which changes the view-stack depth) recovers cleanly.
 */

import React from 'react';
import { Box, Text, useInput } from 'ink';
import pc from 'picocolors';

interface ErrorBoundaryProps {
  children: React.ReactNode;
  /** Changing this value clears a previously-caught error (e.g. on navigation). */
  resetKey?: unknown;
  /** Whether there's a previous view to pop back to. */
  canGoBack: boolean;
  /** Pop the current (crashed) view. */
  onBack: () => void;
  /** Quit the app. */
  onQuit: () => void;
}

interface ErrorBoundaryState {
  error: Error | null;
}

export class ErrorBoundary extends React.Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(): void {
    // State is already captured by getDerivedStateFromError. We deliberately
    // don't write to stdout/stderr here — that would corrupt the alt-screen
    // TUI canvas. The panel below surfaces the message instead.
  }

  componentDidUpdate(prevProps: ErrorBoundaryProps): void {
    if (this.state.error && prevProps.resetKey !== this.props.resetKey) {
      this.setState({ error: null });
    }
  }

  render(): React.ReactNode {
    if (this.state.error) {
      return (
        <ErrorPanel
          error={this.state.error}
          canGoBack={this.props.canGoBack}
          onBack={this.props.onBack}
          onQuit={this.props.onQuit}
        />
      );
    }
    return this.props.children;
  }
}

interface ErrorPanelProps {
  error: Error;
  canGoBack: boolean;
  onBack: () => void;
  onQuit: () => void;
}

function ErrorPanel({ error, canGoBack, onBack, onQuit }: ErrorPanelProps): React.ReactElement {
  useInput((input, key) => {
    if (key.escape && canGoBack) {
      onBack();
    } else if (input === 'q') {
      onQuit();
    }
  });

  const hint = canGoBack ? 'Press Esc to go back · q to quit' : 'Press q to quit';

  return (
    <Box flexDirection="column" paddingX={2} paddingY={1}>
      <Text>{pc.red(pc.bold('✖ This view hit an error'))}</Text>
      <Text> </Text>
      <Text>{pc.white(error.message)}</Text>
      <Text> </Text>
      <Text dimColor>{hint}</Text>
    </Box>
  );
}
