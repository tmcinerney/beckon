{
  config,
  lib,
  pkgs,
  ...
}:

let
  inherit (lib)
    getExe
    makeBinPath
    mkEnableOption
    mkIf
    mkMerge
    mkOption
    types
    ;

  programCfg = config.programs.beckon;
  serviceCfg = config.services.beckond;
  toml = pkgs.formats.toml { };

  stateDirectory = "${serviceCfg.stateHome}/beckon";
  defaultLogDirectory =
    if pkgs.stdenv.hostPlatform.isDarwin then
      "${config.home.homeDirectory}/Library/Logs/beckon"
    else
      "${stateDirectory}/log";

  environment = {
    XDG_STATE_HOME = serviceCfg.stateHome;
    PATH = "${makeBinPath [ serviceCfg.package ]}:${config.home.profileDirectory}/bin:/usr/bin:/bin";
  }
  // serviceCfg.extraEnvironment;

  environmentList = lib.mapAttrsToList (name: value: "${name}=${value}") environment;
in
{
  options = {
    programs.beckon = {
      enable = mkEnableOption "the Beckon command-line client";

      package = mkOption {
        type = types.package;
        default = pkgs.callPackage ./package.nix { };
        defaultText = lib.literalExpression "pkgs.callPackage ./nix/package.nix { }";
        description = "The Beckon package to install.";
      };

      settings = mkOption {
        type = toml.type;
        default = { };
        example = builtins.fromTOML (builtins.readFile ../templates/config.toml);
        description = ''
          Beckon configuration rendered declaratively to
          <filename>$XDG_CONFIG_HOME/beckon/config.toml</filename>. Beckon
          supplies <literal>config_version = 3</literal>; all other fields
          must be explicit. Leave this empty to
          manage its TOML configuration outside Home Manager.
        '';
      };

      devices = mkOption {
        type = types.attrsOf toml.type;
        default = { };
        example.macbook = builtins.fromTOML (builtins.readFile ../templates/macbook.toml);
        description = ''
          Self-contained device definitions written to beckon/config.d/NAME.toml.
          Names are device instance ids. Removing an entry removes that device's
          inputs and output; action/effect references stay local to each file.
        '';
      };
    };

    services.beckond = {
      enable = mkEnableOption "the Beckon background daemon";

      package = mkOption {
        type = types.package;
        default = programCfg.package;
        defaultText = lib.literalExpression "config.programs.beckon.package";
        description = "The Beckon package that provides the beckond daemon.";
      };

      stateHome = mkOption {
        type = types.str;
        default = config.xdg.stateHome;
        defaultText = lib.literalExpression "config.xdg.stateHome";
        description = ''
          Parent directory for Beckon's durable state. Beckon stores bindings and
          its Unix socket in a <filename>beckon</filename> child directory.
        '';
      };

      logDirectory = mkOption {
        type = types.str;
        default = defaultLogDirectory;
        defaultText = lib.literalExpression ''"$HOME/Library/Logs/beckon" on Darwin, otherwise "$XDG_STATE_HOME/beckon/log"'';
        description = "Directory for beckond stdout and stderr logs.";
      };

      extraEnvironment = mkOption {
        type = types.attrsOf types.str;
        default = { };
        example = {
          HERDR_SOCKET_PATH = "/run/user/1000/herdr.sock";
        };
        description = "Additional environment variables passed to beckond.";
      };
    };
  };

  config = mkMerge [
    (mkIf programCfg.enable {
      home.packages = [ programCfg.package ];
    })

    (mkIf (programCfg.settings != { }) {
      xdg.configFile."beckon/config.toml" = {
        source = toml.generate "beckon-config.toml" (
          lib.recursiveUpdate programCfg.settings {
            config_version = 3;
          }
        );
      };
    })

    (mkIf (programCfg.devices != { }) {
      assertions = lib.mapAttrsToList (name: _: {
        assertion = builtins.match "[a-z][a-z0-9-]*" name != null;
        message = "Beckon device names must use lowercase kebab-case: ${name}";
      }) programCfg.devices;
      xdg.configFile = lib.mapAttrs' (
        name: value:
        lib.nameValuePair "beckon/config.d/${name}.toml" {
          source = toml.generate "beckon-device-${name}.toml" value;
        }
      ) programCfg.devices;
    })

    (mkIf serviceCfg.enable {
      # AIDEV-NOTE: Beckon derives its state directory as "$XDG_STATE_HOME/beckon".
      # Keep that parent explicit so a managed daemon and interactive CLI share it.
      home.activation.createBeckondDirectories = lib.hm.dag.entryAfter [ "writeBoundary" ] ''
        $DRY_RUN_CMD mkdir -p ${lib.escapeShellArg stateDirectory}
        $DRY_RUN_CMD mkdir -p ${lib.escapeShellArg serviceCfg.logDirectory}
      '';

      launchd.agents.beckond = mkIf pkgs.stdenv.hostPlatform.isDarwin {
        enable = true;
        config = {
          Label = "org.beckon.beckond";
          ProgramArguments = [
            (getExe serviceCfg.package)
            "daemon"
          ];
          RunAtLoad = true;
          KeepAlive = true;
          StandardOutPath = "${serviceCfg.logDirectory}/beckond.log";
          StandardErrorPath = "${serviceCfg.logDirectory}/beckond.error.log";
          EnvironmentVariables = environment;
        };
      };

      systemd.user.services.beckond = mkIf pkgs.stdenv.hostPlatform.isLinux {
        Unit = {
          Description = "Beckon agent-pane navigation daemon";
          After = [ "graphical-session-pre.target" ];
          PartOf = [ "graphical-session.target" ];
        };
        Service = {
          ExecStart = "${getExe serviceCfg.package} daemon";
          Restart = "on-failure";
          RestartSec = 2;
          Environment = environmentList;
          StandardOutput = "append:${serviceCfg.logDirectory}/beckond.log";
          StandardError = "append:${serviceCfg.logDirectory}/beckond.error.log";
        };
        Install.WantedBy = [ "graphical-session.target" ];
      };
    })
  ];
}
