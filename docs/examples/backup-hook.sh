runuser -u kajiba-server -- sh -eu <<'SH'
umask 077
dump_root=/var/lib/kajiba-dumps
handoff=/var/lib/kajiba-backup-handoff
exec 9>"$dump_root/.copy.lock"
flock -x 9
dump=$(kajiba-server admin dump)
cp -a -- "$dump" "$handoff/"
SH
