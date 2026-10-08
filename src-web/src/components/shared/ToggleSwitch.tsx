import React from "react";

interface ToggleSwitchProps {
  checked: boolean;
  onChange?: (next: boolean) => void;
  disabled?: boolean;
  label?: string;
}

export const ToggleSwitch: React.FC<ToggleSwitchProps> = ({ checked, onChange, disabled, label }) => (
  <span
    className={`toggle-switch ${checked ? "on" : ""} ${disabled ? "disabled" : ""}`}
    role="switch"
    aria-checked={checked}
    aria-label={label}
    onClick={() => !disabled && onChange?.(!checked)}
  />
);
