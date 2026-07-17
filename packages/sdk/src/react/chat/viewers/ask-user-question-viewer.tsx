/**
 * AskUserQuestionViewer Component
 *
 * Displays user question dialogs with options.
 */

import React, { memo } from 'react';
import { HelpCircle } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants, badgeVariants } from '../variants';

interface QuestionOption {
  label: string;
  description?: string;
}

interface Question {
  question: string;
  header?: string;
  options: QuestionOption[];
  multiSelect?: boolean;
}

interface AskUserQuestionViewerProps {
  input: Record<string, unknown>;
}

const questionColor = '#f59e0b';

export const AskUserQuestionViewer = memo(function AskUserQuestionViewer({ input }: AskUserQuestionViewerProps) {
  const questions = (input.questions as Question[]) || [];

  return (
    <div className={cn(chatCardVariants())}>
      {/* Header */}
      <div className={cn(chatCardHeaderVariants())}>
        <HelpCircle size={14} style={{ color: questionColor }} />
        <span className="text-[11px] font-bold text-foreground">Question</span>
        {questions.length > 1 && (
          <span
            className={cn(badgeVariants({ variant: 'colored' }))}
            style={{
              backgroundColor: `${questionColor}15`,
              color: questionColor,
            }}
          >
            {questions.length} questions
          </span>
        )}
      </div>

      {/* Questions */}
      <div className="px-3 py-2 space-y-2">
        {questions.map((q, i) => (
          <div key={i}>
            {/* Question header */}
            {q.header && (
              <span className="text-[9px] font-bold uppercase tracking-wider" style={{ color: questionColor }}>
                {q.header}
              </span>
            )}
            {/* Question text */}
            <div className="text-[11px] mb-1 text-foreground">{q.question}</div>
            {/* Options */}
            <div className="flex flex-wrap gap-1">
              {q.options.map((opt, j) => (
                <span key={j} className={cn(badgeVariants())} title={opt.description}>
                  {opt.label}
                </span>
              ))}
              {q.multiSelect && (
                <span className="px-1.5 py-0.5 rounded text-[8px] italic text-muted-foreground">(multi-select)</span>
              )}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
});
