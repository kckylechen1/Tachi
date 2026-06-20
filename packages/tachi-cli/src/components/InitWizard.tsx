import React, { useState } from 'react';
import { Box, Text, useInput, useApp } from 'ink';
import { banner, colors } from '../utils/ui.js';
import { t, setLanguage, type Language } from '../utils/i18n.js';
import { saveConfig, loadConfig } from '../utils/config.js';
import { startDaemon } from '../utils/daemon.js';
import { MaskedInput } from './MaskedInput.js';
import { PROVIDER_KEYS, vaultSet } from '../utils/vault.js';

type Step = 'language' | 'vaultPassword' | 'keys' | 'complete';

interface KeyResult {
  name: string;
  status: 'saved' | 'skipped' | 'failed';
  error?: string;
}

export function InitWizard() {
  const { exit } = useApp();
  const [step, setStep] = useState<Step>('language');
  const [selectedLang, setSelectedLang] = useState<Language>('en');
  const [selectedIndex, setSelectedIndex] = useState(0);

  // Key-entry state. The vault master password and each secret value live only
  // in component state long enough to be piped to `tachi vault set` over stdin.
  const [vaultPassword, setVaultPassword] = useState('');
  const [keyIndex, setKeyIndex] = useState(0);
  const [saving, setSaving] = useState(false);
  const [results, setResults] = useState<KeyResult[]>([]);

  const languages = [
    { code: 'en' as Language, label: 'English' },
    { code: 'zh' as Language, label: '中文' },
  ];

  // Persist config + auto-start the daemon, then exit. Shared by the "skip keys"
  // and "keys done" transitions so the original behavior is preserved exactly.
  const finishAndStartDaemon = () => {
    setStep('complete');
    setTimeout(async () => {
      await startDaemon();
      exit();
    }, 2000);
  };

  const recordResult = (result: KeyResult) => {
    setResults((prev) => [...prev, result]);
  };

  // Advance past the just-handled key; finish once every provider key is done.
  const advanceKey = () => {
    setKeyIndex((prev) => {
      const next = prev + 1;
      if (next >= PROVIDER_KEYS.length) {
        finishAndStartDaemon();
      }
      return next;
    });
  };

  const handleKeySubmit = (value: string) => {
    if (saving) return;
    const spec = PROVIDER_KEYS[keyIndex];
    if (!spec) return;

    // Empty input = skip this key.
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
    (input, key) => {
      if (step === 'language') {
        if (key.upArrow) {
          setSelectedIndex((prev) => (prev > 0 ? prev - 1 : languages.length - 1));
        } else if (key.downArrow) {
          setSelectedIndex((prev) => (prev < languages.length - 1 ? prev + 1 : 0));
        } else if (key.return) {
          const lang = languages[selectedIndex].code;
          setSelectedLang(lang);
          setLanguage(lang);

          // Save config (unchanged behavior).
          const config = loadConfig();
          config.ui.language = lang;
          saveConfig(config);

          setStep('vaultPassword');
        } else if (key.escape || input === 'q') {
          exit();
        }
        return;
      }

      // On the password step, Esc skips key entry entirely and proceeds exactly
      // like the original wizard (language + daemon only).
      if (step === 'vaultPassword' && key.escape) {
        finishAndStartDaemon();
        return;
      }

      // On the keys step, Esc skips all remaining keys.
      if (step === 'keys' && key.escape && !saving) {
        for (let i = keyIndex; i < PROVIDER_KEYS.length; i++) {
          recordResult({ name: PROVIDER_KEYS[i].name, status: 'skipped' });
        }
        finishAndStartDaemon();
        return;
      }
    },
    // Keep this handler active only when MaskedInput is NOT capturing input, so
    // the two don't both consume the same keystrokes.
    { isActive: step === 'language' || step === 'vaultPassword' || step === 'keys' }
  );

  const handlePasswordSubmit = (value: string) => {
    // Empty master password = skip all key entry (backward-compatible path).
    if (value.trim().length === 0) {
      finishAndStartDaemon();
      return;
    }
    setVaultPassword(value);
    setStep('keys');
  };

  return (
    <Box flexDirection="column" padding={1}>
      <Text>{banner}</Text>

      {step === 'language' && (
        <Box flexDirection="column" marginTop={1}>
          <Box borderStyle="round" borderColor="cyan" paddingX={2} paddingY={1} flexDirection="column">
            <Text bold>{t('init.welcome')}</Text>
            <Text dimColor>{t('init.setup')}</Text>

            <Box flexDirection="column" marginTop={1}>
              <Text>{t('settings.language')}:</Text>
              {languages.map((lang, index) => (
                <Text key={lang.code}>
                  {index === selectedIndex ? colors.primary('❯ ') : '  '}
                  {index === selectedIndex ? colors.primary(lang.label) : lang.label}
                  {lang.code === selectedLang && colors.success(' ✓')}
                </Text>
              ))}
            </Box>
          </Box>

          <Box marginTop={1}>
            <Text dimColor>↑↓ Navigate | Enter Confirm | q Quit</Text>
          </Box>
        </Box>
      )}

      {step === 'vaultPassword' && (
        <Box flexDirection="column" marginTop={1}>
          <Box borderStyle="round" borderColor="cyan" paddingX={2} paddingY={1} flexDirection="column">
            <Text bold>{t('init.keysTitle')}</Text>
            <Text dimColor>{t('init.keysIntro')}</Text>

            <Box marginTop={1} flexDirection="column">
              <Text>{t('init.vaultPassword')}:</Text>
              <Box>
                <Text>{'> '}</Text>
                <MaskedInput onSubmit={handlePasswordSubmit} placeholder="••••••••" />
              </Box>
              <Text dimColor>{t('init.vaultPasswordHint')}</Text>
            </Box>
          </Box>

          <Box marginTop={1}>
            <Text dimColor>{t('init.keySkip')} (empty) | {t('init.keysSkipAll')}</Text>
          </Box>
        </Box>
      )}

      {step === 'keys' && keyIndex < PROVIDER_KEYS.length && (
        <Box flexDirection="column" marginTop={1}>
          <Box borderStyle="round" borderColor="cyan" paddingX={2} paddingY={1} flexDirection="column">
            <Text bold>{t('init.keysTitle')}</Text>

            {results.map((r) => (
              <Text key={r.name}>
                {r.status === 'saved' && colors.success(`✓ ${r.name} ${t('init.keySaved')}`)}
                {r.status === 'skipped' && colors.dim(`- ${r.name}`)}
                {r.status === 'failed' &&
                  colors.error(`✗ ${r.name} ${t('init.keyFailed')}: ${r.error ?? ''}`)}
              </Text>
            ))}

            <Box marginTop={1} flexDirection="column">
              <Text>
                {colors.primary(PROVIDER_KEYS[keyIndex].label)} ({PROVIDER_KEYS[keyIndex].name})
              </Text>
              {PROVIDER_KEYS[keyIndex].hint && (
                <Text dimColor>{PROVIDER_KEYS[keyIndex].hint}</Text>
              )}
              <Box>
                <Text>{`${t('init.keyEnter')}: `}</Text>
                {saving ? (
                  <Text dimColor>{t('init.keySaving')}</Text>
                ) : (
                  <MaskedInput
                    key={keyIndex}
                    onSubmit={handleKeySubmit}
                    placeholder=""
                    isActive={!saving}
                  />
                )}
              </Box>
            </Box>
          </Box>

          <Box marginTop={1}>
            <Text dimColor>{t('init.keySkip')} (empty) | {t('init.keysSkipAll')}</Text>
          </Box>
        </Box>
      )}

      {step === 'complete' && (
        <Box flexDirection="column" marginTop={1}>
          <Box borderStyle="round" borderColor="green" paddingX={2} paddingY={1} flexDirection="column">
            <Text>{colors.success('✓ ' + t('init.complete'))}</Text>
            {results.some((r) => r.status === 'saved') && (
              <Text dimColor>
                {results.filter((r) => r.status === 'saved').length} key(s) stored in vault.
              </Text>
            )}
            <Box marginTop={1}>
              <Text dimColor>{t('init.startNow')}...</Text>
            </Box>
          </Box>
        </Box>
      )}
    </Box>
  );
}
