#!/bin/bash
OLD_INDEX=$(find ../jinx-backups/ -name 'jinx2.*.sqlite' | rg --only-matching --replace '$1' '/jinx2\.(\d+)\.sqlite$' | sort --numeric-sort --reverse | head -n1)
NEW_INDEX=$(($OLD_INDEX + 1))
sqlite3 jinx2.sqlite "VACUUM"
sqlite3 -readonly jinx2.sqlite ".backup '../jinx-backups/jinx2.$NEW_INDEX.sqlite'"
du -h ../jinx-backups/jinx2.$OLD_INDEX.sqlite
du -h ../jinx-backups/jinx2.$NEW_INDEX.sqlite
df -h .
