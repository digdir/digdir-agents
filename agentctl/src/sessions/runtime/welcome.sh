# Installed by agentd: a reminder of how sessions and tmux work, for interactive login shells.
case $- in
*i*) ;;
*) return 0 ;;
esac
if [ -n "${TMUX:-}" ]; then
    cat <<'WELCOME'
This shell is a tmux window beside the session's harness.
  Ctrl-b n / Ctrl-b p   switch between windows     Ctrl-b d   detach, leaving it running
  exit                  close this shell
WELCOME
else
    cat <<'WELCOME'
Sessions run in tmux. `agent-session` lists them, and `agent-session NAME` attaches.
Once attached:
  Ctrl-b d   detach, leaving it running     Ctrl-b c   open a shell beside it
  Ctrl-b n   next window                    mouse wheel  scroll back
WELCOME
fi
