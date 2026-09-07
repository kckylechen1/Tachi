import React, { useState, useEffect } from 'react';
import { Box, Text, useInput } from 'ink';
import { t } from '../utils/i18n.js';
import { colors } from '../utils/ui.js';
import { getSkills, toggleSkill, type Skill } from '../utils/skills.js';

interface SkillsManagerProps {
  onBack: () => void;
}

export function SkillsManager({ onBack }: SkillsManagerProps) {
  const [skills, setSkills] = useState<Skill[]>([]);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [loading, setLoading] = useState(true);
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');

  useEffect(() => {
    loadSkills();
  }, []);

  const loadSkills = async () => {
    setLoading(true);
    try {
      const skillList = await getSkills();
      setSkills(skillList);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load skills');
    }
    setLoading(false);
  };

  const handleToggle = async () => {
    if (skills.length === 0) return;
    const skill = skills[selectedIndex];
    try {
      await toggleSkill(skill.id, !skill.enabled);
      await loadSkills();
      setMessage(`${skill.name} ${!skill.enabled ? 'enabled' : 'disabled'}`);
      setTimeout(() => setMessage(''), 2000);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to toggle skill');
      setTimeout(() => setError(''), 3000);
    }
  };

  useInput((input, key) => {
    if (loading) return;
    
    if (key.upArrow) {
      if (skills.length > 0) {
        setSelectedIndex(prev => (prev > 0 ? prev - 1 : skills.length - 1));
      }
    } else if (key.downArrow) {
      if (skills.length > 0) {
        setSelectedIndex(prev => (prev < skills.length - 1 ? prev + 1 : 0));
      }
    } else if (input === 'e') {
      handleToggle();
    } else if (input === 'r') {
      loadSkills();
    } else if (input === 'q' || key.escape) {
      onBack();
    }
  });

  return (
    <Box flexDirection="column" padding={1}>
      <Box flexDirection="column" borderStyle="round" borderColor="cyan" paddingX={2} paddingY={1}>
        <Text bold>{t('skills.title')}</Text>
        
        {message && (
          <Box marginTop={1}>
            <Text>{colors.success(message)}</Text>
          </Box>
        )}

        {error && (
          <Box marginTop={1}>
            <Text>{colors.error(`✗ ${error}`)}</Text>
          </Box>
        )}

        <Box flexDirection="column" marginTop={1}>
          {loading ? (
            <Text dimColor>Loading skills...</Text>
          ) : skills.length === 0 ? (
            <Text dimColor>No skills found. Register skills via daemon.</Text>
          ) : (
            skills.map((skill, index) => (
              <Box key={skill.id} flexDirection="column">
                <Text>
                  {index === selectedIndex ? colors.primary('❯ ') : '  '}
                  {skill.enabled ? colors.success('●') : colors.error('○')} {skill.name}
                  <Text dimColor>  v{skill.version || 1}</Text>
                </Text>
                {skill.description && index === selectedIndex && (
                  <Text dimColor>    {skill.description}</Text>
                )}
                {index === selectedIndex && skill.uses !== undefined && (
                  <Text dimColor>    Uses: {skill.uses} | Success: {skill.successes || 0} | Fail: {skill.failures || 0}</Text>
                )}
              </Box>
            ))
          )}
        </Box>

        {!loading && (
          <Box flexDirection="column" marginTop={2}>
            <Text dimColor>Commands:</Text>
            <Text dimColor>  e - Enable/Disable selected</Text>
            <Text dimColor>  r - Refresh list</Text>
            <Text dimColor>  q - Back</Text>
          </Box>
        )}
      </Box>
    </Box>
  );
}
