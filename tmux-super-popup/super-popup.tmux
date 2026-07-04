set-option -goq @super-popup-session __tmux_super_popup
set-option -goq @super-popup-width 96%
set-option -goq @super-popup-height 92%
set-option -goqF @super-popup-script '#{d:current_file}/scripts/launcher.sh'

unbind-key -q -T popup M-a
bind-key -n M-a run-shell -b '#{@super-popup-script} --toggle'
