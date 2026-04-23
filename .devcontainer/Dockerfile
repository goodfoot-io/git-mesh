FROM node:24

ARG TZ
ENV TZ="$TZ"

# Install basic development tools
RUN apt update && export DEBIAN_FRONTEND=noninteractive && apt install -y less \
  build-essential \
  curl \
  git \
  procps \
  chromium \
  sudo \
  fzf \
  zsh \
  man-db \
  unzip \
  gnupg2 \
  gh \
  jq \
  tree \
  locales \
  git-lfs \
  ripgrep \
  shellcheck \
  dbus \
  dbus-x11 \
  xvfb \
  bc \
  bat \
  wget

# Configure locale
RUN sed -i '/en_US.UTF-8/s/^# //g' /etc/locale.gen && \
  locale-gen
ENV LANG=en_US.UTF-8
ENV LANGUAGE=en_US:en
ENV LC_ALL=en_US.UTF-8

# Ensure default node user has access to /usr/local/share
RUN mkdir -p /usr/local/share/npm-global && \
  chown -R node:node /usr/local/share

ARG USERNAME=node

# Persist bash history.
RUN SNIPPET="export PROMPT_COMMAND='history -a' && export HISTFILE=/commandhistory/.bash_history" \
  && mkdir /commandhistory \
  && touch /commandhistory/.bash_history \
  && chown -R $USERNAME /commandhistory

# Set `DEVCONTAINER` environment variable to help with orientation
ENV DEVCONTAINER=true

# Create workspace and config directories with permissions
RUN mkdir -p /workspace /home/node/.claude && \
  chown -R node:node /workspace /home/node/.claude

WORKDIR /workspace

# Set up non-root user
USER node

# Install Rust toolchain for the node user
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
ENV PATH=$PATH:/home/node/.cargo/bin

# Configure Yarn to use workspace-local directories
ENV YARN_CACHE_FOLDER=/workspace/.yarn/cache
ENV YARN_GLOBAL_FOLDER=/workspace/.yarn/global

# Install global packages
ENV NPM_CONFIG_PREFIX=/usr/local/share/npm-global
ENV PATH=$PATH:/usr/local/share/npm-global/bin
ENV NODE_OPTIONS=--max-old-space-size=4096

# Set the default shell to zsh rather than sh
ENV SHELL=/bin/zsh

# Default powerline10k theme
RUN sh -c "$(wget -O- https://github.com/deluan/zsh-in-docker/releases/download/v1.2.0/zsh-in-docker.sh)" -- \
  -p git \
  -p fzf \
  -a "source /usr/share/doc/fzf/examples/key-bindings.zsh" \
  -a "source /usr/share/doc/fzf/examples/completion.zsh" \
  -a "export PROMPT_COMMAND='history -a' && export HISTFILE=/commandhistory/.bash_history" \
  -x

ENV CLAUDE_BASH_MAINTAIN_PROJECT_WORKING_DIR=1

# Install Claude and common dev tools
RUN npm install -g @anthropic-ai/claude-code @google/gemini-cli @openai/codex

# Install tsx for running TypeScript scripts
RUN npm install -g tsx

# Install dotenv-cli for loading environment variables
RUN npm install -g dotenv-cli

# Install tools for code analysis
RUN npm install -g @ast-grep/cli jsdoczoom

USER root

RUN corepack enable

RUN echo "node ALL=(root) NOPASSWD: /usr/bin/apt, /usr/bin/apt-get, /usr/bin/dpkg" > /etc/sudoers.d/node-packages && \
  echo "node ALL=(root) NOPASSWD: /usr/bin/dbus-daemon, /usr/bin/mkdir, /usr/bin/chmod, /usr/bin/chown, /usr/bin/pgrep, /usr/bin/sleep" > /etc/sudoers.d/node-dbus && \
  chmod 0440 /etc/sudoers.d/node-packages /etc/sudoers.d/node-dbus

# Create dbus directories with proper permissions
RUN mkdir -p /run/dbus /var/run/dbus && \
  chmod 755 /run/dbus /var/run/dbus

USER node
