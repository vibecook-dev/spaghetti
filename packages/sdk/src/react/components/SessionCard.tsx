import React from 'react';
import type { SessionListItem } from '../../index.js';
import { formatRelativeTime, formatTokenCount, formatDuration } from '../utils/formatters.js';

export function SessionCard({
  session,
  isSelected,
  onClick,
  onTodosClick,
  onPlanClick,
  onTaskClick,
}: {
  session: SessionListItem;
  isSelected: boolean;
  onClick: () => void;
  onTodosClick?: () => void;
  onPlanClick?: () => void;
  onTaskClick?: () => void;
}) {
  const totalTokens = session.tokenUsage.totalTokens;

  return (
    <button
      onClick={onClick}
      className={`w-full text-left px-3 py-2 border-b border-white/5 transition-colors cursor-pointer ${
        isSelected ? 'bg-white/10' : 'hover:bg-white/5'
      }`}
    >
      <div className="flex items-center gap-2 mb-0.5">
        <span className="text-xs font-mono text-white/60">{session.sessionId.slice(0, 8)}</span>
        {session.gitBranch && (
          <span className="text-[10px] bg-purple-500/20 text-purple-300 px-1 py-0.5 rounded">{session.gitBranch}</span>
        )}
        {session.isSidechain && (
          <span className="text-[10px] bg-yellow-500/20 text-yellow-300 px-1 py-0.5 rounded">sidechain</span>
        )}
      </div>

      {session.summary && <div className="text-xs text-white/70 mb-0.5 truncate">{session.summary}</div>}
      {!session.summary && session.firstPrompt && (
        <div className="text-xs text-white/50 mb-0.5 truncate italic">{session.firstPrompt}</div>
      )}

      <div className="flex gap-3 text-[10px] text-white/40">
        <span>{session.messageCount} msgs</span>
        <span>{formatTokenCount(totalTokens)} tokens</span>
        <span>{formatDuration(session.lifespanMs)}</span>
      </div>

      <div className="flex gap-2 text-[10px] mt-0.5 flex-wrap">
        <span className="text-white/30">{formatRelativeTime(session.lastUpdate)}</span>
        {session.todoCount > 0 && (
          <span
            className="bg-green-500/20 text-green-300 px-1 py-0.5 rounded hover:bg-green-500/30 cursor-pointer"
            onClick={(e) => {
              e.stopPropagation();
              onTodosClick?.();
            }}
          >
            {session.todoCount} todos
          </span>
        )}
        {session.hasTask && (
          <span
            className="bg-orange-500/20 text-orange-300 px-1 py-0.5 rounded hover:bg-orange-500/30 cursor-pointer"
            onClick={(e) => {
              e.stopPropagation();
              onTaskClick?.();
            }}
          >
            task
          </span>
        )}
        {session.planSlug && (
          <span
            className="bg-cyan-500/20 text-cyan-300 px-1 py-0.5 rounded hover:bg-cyan-500/30 cursor-pointer"
            onClick={(e) => {
              e.stopPropagation();
              onPlanClick?.();
            }}
          >
            plan
          </span>
        )}
      </div>
    </button>
  );
}
