import React from "react";

interface TargetBadgeProps {
  targetLabel: string;
  isRemote: boolean;
  onChangeTarget?: () => void;
}

/** Auto-bound target hint -- the coding target is inherited from the
 * currently open workspace, not a dropdown to pick from each time. */
export const TargetBadge: React.FC<TargetBadgeProps> = ({ targetLabel, isRemote, onChangeTarget }) => (
  <span style={{ display: "inline-flex", alignItems: "center", gap: 8 }}>
    <span className={`target-badge ${isRemote ? "remote" : "local"}`}>
      {isRemote ? "🖥" : "💻"} 目标: {targetLabel}
    </span>
    {onChangeTarget && <button className="target-change-link" onClick={onChangeTarget}>更改▾</button>}
  </span>
);
