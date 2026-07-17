/**
 * RawJsonViewer Component
 *
 * Collapsible JSON viewer for debugging raw message data.
 */

import React, { useState, memo } from 'react';
import { Code } from 'lucide-react';

interface RawJsonViewerProps {
  data: unknown;
  minimal?: boolean;
}

export const RawJsonViewer = memo(function RawJsonViewer({ data, minimal = false }: RawJsonViewerProps) {
  const [isOpen, setIsOpen] = useState(false);

  return (
    <div className={minimal ? 'inline-block ml-2' : 'mt-2'}>
      <button
        onClick={(e) => {
          e.stopPropagation();
          setIsOpen(!isOpen);
        }}
        className="text-[10px] font-mono text-muted-foreground opacity-60 hover:opacity-100 flex items-center gap-1 transition-opacity border border-transparent hover:border-border rounded px-1"
      >
        <Code size={10} />
        {minimal ? '' : isOpen ? 'Hide' : 'JSON'}
      </button>

      {isOpen && (
        <div className="mt-2 p-3 border border-border overflow-auto relative z-20 text-left shadow-lg max-h-80 bg-card rounded-lg">
          <pre className="text-[10px] font-mono leading-tight whitespace-pre-wrap break-all text-foreground">
            {JSON.stringify(data, null, 2)}
          </pre>
        </div>
      )}
    </div>
  );
});
