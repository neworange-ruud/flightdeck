# The worker's environment for every shell, SSH sessions included (they do
# not get the container's ENV).
export CLAUDE_CONFIG_DIR=/home/dev/.claude
export XDG_CACHE_HOME=/home/dev/.cache
export TERM="${TERM:-xterm-256color}"
# A UTF-8 locale, as a real server has (nano draws an em dash as ^'^t without).
export LANG=C.UTF-8
