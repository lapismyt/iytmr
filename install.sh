#!/bin/bash

# Exit on error
set -e

PROJECT_NAME="iytmr"
SERVICE_FILE="/etc/systemd/system/${PROJECT_NAME}.service"
WORKING_DIR=$(pwd)
USER=$(whoami)

echo "--- Updating repository ---"
if [ -d .git ]; then
    git pull
else
    echo "Not a git repository, skipping pull."
fi

echo "--- Building in release mode ---"
cargo build --release

echo "--- Checking .env file ---"
if [ ! -f .env ]; then
    if [ -f .env.example ]; then
        echo "Creating .env from .env.example..."
        cp .env.example .env
        echo "!!! Please edit .env and add your TELOXIDE_TOKEN !!!"
    else
        echo "Warning: .env.example not found. Ensure .env exists before starting the service."
    fi
fi

echo "--- Checking service file ---"
if [ ! -f "$SERVICE_FILE" ]; then
    echo "Creating systemd service file at $SERVICE_FILE..."

    sudo bash -c "cat > $SERVICE_FILE" <<EOF
[Unit]
Description=iytmr Telegram Bot Service
After=network.target

[Service]
Type=simple
User=$USER
WorkingDirectory=$WORKING_DIR
ExecStart=$WORKING_DIR/target/release/$PROJECT_NAME
EnvironmentFile=$WORKING_DIR/.env
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

    echo "Reloading systemd and enabling service..."
    sudo systemctl daemon-reload
    sudo systemctl enable $PROJECT_NAME
    echo "Service created and enabled."
    echo "To start the service, run: sudo systemctl start $PROJECT_NAME"
else
    echo "Service file already exists at $SERVICE_FILE."
    echo "Restarting service to apply updates..."
    sudo systemctl restart $PROJECT_NAME
fi

echo "--- Done! ---"
