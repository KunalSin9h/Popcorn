# Every login shell in the case: route traffic through the host and use a placeholder key.
export HTTP_PROXY=http://127.0.0.1:3128 HTTPS_PROXY=http://127.0.0.1:3128
export http_proxy=$HTTP_PROXY https_proxy=$HTTPS_PROXY NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost
export ANTHROPIC_BASE_URL=http://127.0.0.1:8080
export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 DISABLE_AUTOUPDATER=1 NPM_CONFIG_UPDATE_NOTIFIER=false
if [ "$(cat /run/popcorn-auth 2>/dev/null)" = oauth ]; then
  export CLAUDE_CODE_OAUTH_TOKEN=popcorn-placeholder-the-host-holds-the-real-token
else
  export ANTHROPIC_API_KEY=popcorn-placeholder-the-host-holds-the-real-key
fi
cd ~/work 2>/dev/null
