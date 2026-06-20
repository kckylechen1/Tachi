import React, { useState } from 'react';
import { Text, useInput } from 'ink';

interface MaskedInputProps {
  /** Called with the typed value when the user presses Enter. */
  onSubmit: (value: string) => void;
  /** Mask character rendered in place of each typed character. */
  mask?: string;
  /** Placeholder shown (dimmed) when the buffer is empty. */
  placeholder?: string;
  /** When false the component ignores keystrokes (e.g. while submitting). */
  isActive?: boolean;
}

/**
 * Minimal masked text input built on Ink's `useInput`. We avoid adding a new
 * `ink-text-input` dependency (none is present in package.json) and never echo
 * the typed characters — each is rendered as `mask` so secrets are not shown on
 * screen. The plaintext value is kept only in component state and handed to
 * `onSubmit`; the caller is responsible for forwarding it to the vault and not
 * persisting it elsewhere.
 */
export function MaskedInput({
  onSubmit,
  mask = '*',
  placeholder = '',
  isActive = true,
}: MaskedInputProps) {
  const [value, setValue] = useState('');

  useInput(
    (input, key) => {
      if (key.return) {
        onSubmit(value);
        setValue('');
        return;
      }
      if (key.backspace || key.delete) {
        setValue((prev) => prev.slice(0, -1));
        return;
      }
      // Ignore control keys / escape sequences; only append printable input.
      // Ink delivers pasted/typed runs in `input`; filter out non-printable.
      if (input && !key.ctrl && !key.meta && !key.escape) {
        // Drop ASCII control chars (< 0x20) and DEL (0x7f); keep everything
        // printable, including pasted multi-character runs.
        const printable = Array.from(input)
          .filter((ch) => {
            const code = ch.codePointAt(0) ?? 0;
            return code >= 0x20 && code !== 0x7f;
          })
          .join('');
        if (printable.length > 0) {
          setValue((prev) => prev + printable);
        }
      }
    },
    { isActive }
  );

  if (value.length === 0) {
    return <Text dimColor>{placeholder}</Text>;
  }
  return <Text>{mask.repeat(value.length)}</Text>;
}
