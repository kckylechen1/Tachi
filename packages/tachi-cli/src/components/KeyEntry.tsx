import React, { useState } from 'react';
import { Box, Text, useInput } from 'ink';
import { colors } from '../utils/ui.js';
import { t } from '../utils/i18n.js';
import { MaskedInput } from './MaskedInput.js';
import { PROVIDER_KEYS, vaultSet } from '../utils/vault.js';

interface KeyEntryProps {
  /** Called when the user finishes or cancels key entry. */
  onDone: () => void;
}

type Stage = 'password' | 'keys' | 'summary';

interface KeyResult {
  name: string;
  status: 'saved' | 'skipped' | 'failed';
  error?: string;
}

/**
 * Standalone provider-key entry flow, reusable from the Settings menu. Mirrors
 * the InitWizard key step but returns control to its parent (`onDone`) instead
 * of starting the daemon. Secrets are piped to `tachi vault set` over stdin and
 * are never rendered or logged.
 */
export function KeyEntry({ onDone }: KeyEntryProps) {
  const [stage, setStage] = useState<Stage>('password');
  const [vaultPassword, setVaultPassword] = useState('');
  const [keyIndex, setKeyIndex] = useState(0);
  const [saving, setSaving] = useState(false);
  const [results, setResults] = useState<KeyResult[]>([]);

  const recordResult = (result: KeyResult) =>
    setResults((prev) => [...prev, result]);

  const advanceKey = () => {
    setKeyIndex((prev) => {
      const next = prev + 1;
      if (next >= PROVIDER_KEYS.length) {
        setStage('summary');
      }
      return next;
    });
  };

  const handlePasswordSubmit = (value: string) => {
    if (value.trim().length === 0) {
      onDone();
      return;
    }
    setVaultPassword(value);
    setStage('keys');
  };

  const handleKeySubmit = (value: string) => {
    if (saving) return;
    const spec = PROVIDER_KEYS[keyIndex];
    if (!spec) return;
    if (value.trim().length === 0) {
      recordResult({ name: spec.name, status: 'skipped' });
      advanceKey();
      return;
    }
    setSaving(true);
    void vaultSet(spec.name, value, { password: vaultPassword }).then((res) => {
      recordResult(
        res.success
          ? { name: spec.name, status: 'saved' }
          : { name: spec.name, status: 'failed', error: res.error }
      );
      setSaving(false);
      advanceKey();
    });
  };

  useInput(
    (_input, key) => {
      if (key.escape && !saving) {
        if (stage === 'summary') {
          onDone();
          return;
        }
        if (stage === 'password') {
          onDone();
          return;
        }
        // stage === 'keys': skip remaining keys then show summary.
        for (let i = keyIndex; i < PROVIDER_KEYS.length; i++) {
          recordResult({ name: PROVIDER_KEYS[i].name, status: 'skipped' });
        }
        setStage('summary');
        return;
      }
      if (stage === 'summary' && key.return) {
        onDone();
      }
    },
    { isActive: true }
  );

  return (
    <Box flexDirection="column" padding={1}>
      <Box flexDirection="column" borderStyle="round" borderColor="cyan" paddingX={2} paddingY={1}>
        <Text bold>{t('init.keysTitle')}</Text>

        {stage === 'password' && (
          <Box marginTop={1} flexDirection="column">
            <Text dimColor>{t('init.keysIntro')}</Text>
            <Text>{t('init.vaultPassword')}:</Text>
            <Box>
              <Text>{'> '}</Text>
              <MaskedInput onSubmit={handlePasswordSubmit} placeholder="••••••••" />
            </Box>
            <Text dimColor>{t('init.vaultPasswordHint')}</Text>
          </Box>
        )}

        {stage === 'keys' && keyIndex < PROVIDER_KEYS.length && (
          <Box marginTop={1} flexDirection="column">
            {results.map((r) => (
              <Text key={r.name}>
                {r.status === 'saved' && colors.success(`✓ ${r.name} ${t('init.keySaved')}`)}
                {r.status === 'skipped' && colors.dim(`- ${r.name}`)}
                {r.status === 'failed' &&
                  colors.error(`✗ ${r.name} ${t('init.keyFailed')}: ${r.error ?? ''}`)}
              </Text>
            ))}
            <Text>
              {colors.primary(PROVIDER_KEYS[keyIndex].label)} ({PROVIDER_KEYS[keyIndex].name})
            </Text>
            {PROVIDER_KEYS[keyIndex].hint && <Text dimColor>{PROVIDER_KEYS[keyIndex].hint}</Text>}
            <Box>
              <Text>{`${t('init.keyEnter')}: `}</Text>
              {saving ? (
                <Text dimColor>{t('init.keySaving')}</Text>
              ) : (
                <MaskedInput key={keyIndex} onSubmit={handleKeySubmit} isActive={!saving} />
              )}
            </Box>
          </Box>
        )}

        {stage === 'summary' && (
          <Box marginTop={1} flexDirection="column">
            {results.map((r) => (
              <Text key={r.name}>
                {r.status === 'saved' && colors.success(`✓ ${r.name} ${t('init.keySaved')}`)}
                {r.status === 'skipped' && colors.dim(`- ${r.name}`)}
                {r.status === 'failed' &&
                  colors.error(`✗ ${r.name} ${t('init.keyFailed')}: ${r.error ?? ''}`)}
              </Text>
            ))}
          </Box>
        )}
      </Box>

      <Box marginTop={1}>
        <Text dimColor>
          {t('init.keySkip')} (empty) | {t('init.keysSkipAll')}
        </Text>
      </Box>
    </Box>
  );
}
