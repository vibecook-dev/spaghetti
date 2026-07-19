import React from 'react';
import type { ProjectListItem } from '../../index.js';
import { formatRelativeTime, formatTokenCount } from '../utils/formatters.js';

export function ProjectCard({
  project,
  isSelected,
  onClick,
  onMemoryClick,
}: {
  project: ProjectListItem;
  isSelected: boolean;
  onClick: () => void;
  onMemoryClick?: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={`w-full text-left px-3 py-2 border-b border-white/5 transition-colors cursor-pointer ${
        isSelected ? 'bg-white/10' : 'hover:bg-white/5'
      }`}
    >
      <div className="flex items-center justify-between mb-0.5">
        <span className="text-xs font-medium text-white truncate">{project.folderName}</span>
        <span className="text-[10px] text-white/40 shrink-0 ml-2">{formatRelativeTime(project.lastActiveAt)}</span>
      </div>
      <div className="text-[10px] text-white/30 truncate mb-1">{project.absolutePath}</div>
      <div className="flex gap-3 text-[10px] text-white/40">
        <span>{project.sessionCount} sessions</span>
        <span>{project.messageCount} msgs</span>
        <span>{formatTokenCount(project.tokenUsage.totalTokens)} tokens</span>
      </div>
      <div className="flex gap-1 mt-1">
        {project.latestGitBranch && (
          <span className="text-[10px] bg-purple-500/20 text-purple-300 px-1 py-0.5 rounded inline-block">
            {project.latestGitBranch}
          </span>
        )}
        {project.hasMemory && (
          <span
            className="text-[10px] bg-blue-500/20 text-blue-300 px-1 py-0.5 rounded inline-block hover:bg-blue-500/30 cursor-pointer"
            onClick={(e) => {
              e.stopPropagation();
              onMemoryClick?.();
            }}
          >
            memory
          </span>
        )}
      </div>
    </button>
  );
}
