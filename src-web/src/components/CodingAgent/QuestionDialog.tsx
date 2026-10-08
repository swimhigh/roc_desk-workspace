import React, { useState } from "react";
import { ConfirmDialog } from "../shared/ConfirmDialog";

interface QuestionDialogProps {
  open: boolean;
  question: string;
  options: string[];
  onAnswer: (answer: string) => void;
}

/** The `question` tool's structured prompt -- genuinely blocks the
 * conversation until the user answers here. No "decline to answer" --
 * unlike `CommandConfirmDialog`, there's no meaningful "reject" tool
 * result for a question. */
export const QuestionDialog: React.FC<QuestionDialogProps> = ({ open, question, options, onAnswer }) => {
  const [text, setText] = useState("");

  React.useEffect(() => {
    if (open) setText("");
  }, [open, question]);

  return (
    <ConfirmDialog
      open={open}
      severity="info"
      icon="❓"
      title="AI 需要你澄清一下"
      dismissible={false}
      actions={
        options.length === 0 ? (
          <button className="btn primary sm" onClick={() => onAnswer(text)} disabled={!text.trim()}>
            提交回答
          </button>
        ) : null
      }
    >
      <p style={{ fontSize: 13, marginBottom: 10 }}>{question}</p>
      {options.length > 0 ? (
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          {options.map((option) => (
            <button key={option} className="btn ghost sm" style={{ justifyContent: "flex-start" }} onClick={() => onAnswer(option)}>
              {option}
            </button>
          ))}
        </div>
      ) : (
        <textarea
          className="form-input"
          rows={3}
          autoFocus
          value={text}
          onChange={(e) => setText(e.target.value)}
          placeholder="输入你的回答…"
        />
      )}
    </ConfirmDialog>
  );
};
