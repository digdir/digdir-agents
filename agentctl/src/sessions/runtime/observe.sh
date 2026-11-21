transcript=$2
# The session's activity moves when an attached client types. A window's moves when what runs in it prints, as the
# harness does while working. Only the harness's window counts, which the launch marks with @agent_harness. Someone may
# open another window, such as a shell with Ctrl-b c, and leave it current, which must neither hide the harness's output
# nor make a quiet harness look busy when it prints, as a dev server does.
values=$(/usr/bin/tmux list-sessions -F '#{session_attached} #{session_activity} #{W:#{?@agent_harness,#{window_activity} ,}}' -f "#{==:#{session_name},$1}")
status=$?
case $status in 0) ;; 1) exit 10 ;; *) exit 11 ;; esac
set -- $values
[ "$#" -eq 0 ] && exit 10
[ "$#" -ge 2 ] || exit 11
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
