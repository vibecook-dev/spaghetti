/**
 * TodoWriteViewer Component
 *
 * Displays task list with status indicators and progress bar.
 */

import React, { memo } from 'react';
import { ListTodo, Circle, Loader2, CheckCircle } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants } from '../variants';
import { toolColors, getStatusColors } from '../theme';

interface TodoItem {
  content: string;
  status: 'pending' | 'in_progress' | 'completed';
  activeForm?: string;
}

interface TodoWriteViewerProps {
  input: Record<string, unknown>;
}

function getStatusIcon(status: string) {
  switch (status) {
    case 'completed':
      return <CheckCircle size={14} className="text-emerald-500" />;
    case 'in_progress':
      return <Loader2 size={14} className="text-amber-500 animate-spin" />;
    default:
      return <Circle size={14} className="text-gray-400" />;
  }
}

export const TodoWriteViewer = memo(function TodoWriteViewer({ input }: TodoWriteViewerProps) {
  const todos = (input.todos as TodoItem[]) || [];

  const completedCount = todos.filter((t) => t.status === 'completed').length;
  const inProgressCount = todos.filter((t) => t.status === 'in_progress').length;
  const pendingCount = todos.filter((t) => t.status === 'pending').length;

  return (
    <div className={cn(chatCardVariants())}>
      {/* Header with stats */}
      <div className={cn(chatCardHeaderVariants())}>
        <ListTodo size={14} style={{ color: toolColors.todo }} />
        <span className="text-[11px] font-bold text-foreground">Task List</span>
        <div className="ml-auto flex items-center gap-3 text-[10px] font-mono">
          {completedCount > 0 && (
            <span className="flex items-center gap-1 text-emerald-500">
              <CheckCircle size={10} /> {completedCount}
            </span>
          )}
          {inProgressCount > 0 && (
            <span className="flex items-center gap-1 text-amber-500">
              <Loader2 size={10} /> {inProgressCount}
            </span>
          )}
          {pendingCount > 0 && (
            <span className="flex items-center gap-1 text-muted-foreground">
              <Circle size={10} /> {pendingCount}
            </span>
          )}
        </div>
      </div>

      {/* Todo items */}
      <div className="divide-y divide-border/50">
        {todos.map((todo, i) => {
          const colors = getStatusColors(todo.status);
          return (
            <div
              key={i}
              className="flex items-start gap-3 px-3 py-2 transition-colors"
              style={{
                backgroundColor: colors.bg,
                borderLeft: `3px solid ${colors.border}`,
              }}
            >
              <div className="shrink-0 mt-0.5">{getStatusIcon(todo.status)}</div>
              <div className="flex-grow min-w-0">
                <div
                  className="text-[11px] leading-snug"
                  style={{
                    color: todo.status === 'completed' ? 'var(--muted-foreground)' : 'var(--foreground)',
                    textDecoration: todo.status === 'completed' ? 'line-through' : 'none',
                    opacity: todo.status === 'completed' ? 0.7 : 1,
                  }}
                >
                  {todo.content}
                </div>
                {todo.status === 'in_progress' && todo.activeForm && (
                  <div className="text-[10px] mt-0.5 font-mono" style={{ color: '#f59e0b' }}>
                    {todo.activeForm}
                  </div>
                )}
              </div>
            </div>
          );
        })}
      </div>

      {/* Progress bar */}
      {todos.length > 0 && (
        <div className="h-1 w-full bg-border">
          <div
            className="h-full transition-all duration-300"
            style={{
              width: `${(completedCount / todos.length) * 100}%`,
              backgroundColor: toolColors.success,
            }}
          />
        </div>
      )}
    </div>
  );
});
