transcript=$2
# The session's activity moves only when an attached client types. A window's moves when what runs in it prints, as the
# harness does while working. Every window counts, since someone may leave another window current, such as a shell, and
# the latest of them all is when the terminal last moved.
values=$(/usr/bin/tmux list-sessions -F '#{session_attached} #{session_activity} #{W:#{window_activity} }' -f "#{==:#{session_name},$1}")
status=$?
case $status in 0) ;; 1) exit 10 ;; *) exit 11 ;; esac
set -- $values
[ "$#" -eq 0 ] && exit 10
[ "$#" -ge 3 ] || exit 11
attached=$1
shift
latest=$1
for activity; do
    [ "$activity" -le "$latest" ] || latest=$activity
done
if [ -f "$transcript" ]; then
    modified=$(/usr/bin/stat -c %Y -- "$transcript") || exit 11
    [ "$modified" -le "$latest" ] || latest=$modified
fi
now=$(/usr/bin/date +%s) || exit 11
age=$((now - latest))
[ "$age" -ge 0 ] || age=0
printf '%s %s\n' "$attached" "$age"
