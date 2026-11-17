# The worker's environment for every shell, SSH sessions included (they do
# not get the container's ENV).
export CLAUDE_CONFIG_DIR=/home/dev/.claude
export XDG_CACHE_HOME=/home/dev/.cache
export TERM="${TERM:-xterm-256color}"
