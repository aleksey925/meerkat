import { useState, useEffect, useRef } from "react";
import DatePicker from "react-datepicker";
import "react-datepicker/dist/react-datepicker.css";

function TimeInput({ hours, minutes, onChangeHours, onChangeMinutes }) {
  const hoursRef = useRef(null);
  const minutesRef = useRef(null);
  const justFocused = useRef(false);

  const handleFocus = (e) => {
    justFocused.current = true;
    e.target.select();
  };

  // prevent mouseup from deselecting after focus
  const handleMouseUp = (e) => {
    if (justFocused.current) {
      justFocused.current = false;
      e.preventDefault();
    }
  };

  const handleHoursChange = (e) => {
    const raw = e.target.value.replace(/\D/g, "").slice(0, 2);
    const num = parseInt(raw, 10);
    if (raw === "" || num <= 23) {
      onChangeHours(raw);
    }
  };

  const handleMinutesChange = (e) => {
    const raw = e.target.value.replace(/\D/g, "").slice(0, 2);
    const num = parseInt(raw, 10);
    if (raw === "" || num <= 59) {
      onChangeMinutes(raw);
    }
  };

  const pad = (val, setter) => {
    if (val.length === 1) setter(val.padStart(2, "0"));
    if (val === "") setter("00");
  };

  const handleHoursKeyDown = (e) => {
    if (e.key === ":" || e.key === "Tab" || e.key === "ArrowRight") {
      e.preventDefault();
      pad(hours, onChangeHours);
      minutesRef.current?.focus();
    } else if (e.key === "Enter") {
      pad(hours, onChangeHours);
    }
  };

  const handleMinutesKeyDown = (e) => {
    if (e.key === "Backspace" && minutes === "") {
      e.preventDefault();
      hoursRef.current?.focus();
    } else if (e.key === "ArrowLeft" && e.target.selectionStart === 0) {
      e.preventDefault();
      hoursRef.current?.focus();
    }
  };

  const fieldStyle = {
    width: 44,
    padding: "6px 0",
    borderRadius: 8,
    border: "0.5px solid var(--border)",
    background: "var(--field-bg)",
    color: "var(--text-primary)",
    fontSize: 18,
    fontFamily: '"SF Mono", ui-monospace, monospace',
    fontWeight: 600,
    outline: "none",
    textAlign: "center",
  };

  return (
    <div style={{ display: "flex", alignItems: "center", justifyContent: "center", gap: 4 }}>
      <input
        ref={hoursRef}
        type="text"
        inputMode="numeric"
        tabIndex={1}
        maxLength={2}
        value={hours}
        placeholder="09"
        onChange={handleHoursChange}
        onFocus={handleFocus}
        onMouseUp={handleMouseUp}
        onBlur={() => pad(hours, onChangeHours)}
        onKeyDown={handleHoursKeyDown}
        style={fieldStyle}
      />
      <span style={{ fontSize: 20, fontWeight: 700, color: "var(--text-secondary)", userSelect: "none" }}>:</span>
      <input
        ref={minutesRef}
        type="text"
        inputMode="numeric"
        tabIndex={2}
        maxLength={2}
        value={minutes}
        placeholder="00"
        onChange={handleMinutesChange}
        onFocus={handleFocus}
        onMouseUp={handleMouseUp}
        onBlur={() => pad(minutes, onChangeMinutes)}
        onKeyDown={handleMinutesKeyDown}
        style={fieldStyle}
      />
    </div>
  );
}

export default function CustomReminderModal({ isOpen, position, onClose, onConfirm }) {
  const [selectedDate, setSelectedDate] = useState(null);
  const [hours, setHours] = useState("09");
  const [minutes, setMinutes] = useState("00");
  const modalRef = useRef(null);

  useEffect(() => {
    if (isOpen) {
      const now = new Date();
      const today = new Date(now);
      today.setHours(0, 0, 0, 0);
      setSelectedDate(today);
      setHours(String(now.getHours()).padStart(2, "0"));
      setMinutes(String(now.getMinutes()).padStart(2, "0"));
    }
  }, [isOpen]);

  useEffect(() => {
    function handleClick(e) {
      if (modalRef.current && !modalRef.current.contains(e.target)) {
        onClose();
      }
    }
    if (isOpen) {
      document.addEventListener("mousedown", handleClick);
      return () => document.removeEventListener("mousedown", handleClick);
    }
  }, [isOpen, onClose]);

  if (!isOpen) return null;

  const handleConfirm = () => {
    if (!selectedDate || hours.length === 0 || minutes.length === 0) return;
    const hh = hours.padStart(2, "0");
    const mm = minutes.padStart(2, "0");
    const y = selectedDate.getFullYear();
    const mo = String(selectedDate.getMonth() + 1).padStart(2, "0");
    const d = String(selectedDate.getDate()).padStart(2, "0");
    const date = `${y}-${mo}-${d}`;
    const dt = new Date(selectedDate);
    dt.setHours(parseInt(hh), parseInt(mm), 0, 0);
    const isoDate = dt.toISOString();
    const display = `${date} ${hh}:${mm}`;
    onConfirm(display, isoDate);
    onClose();
  };

  const style = {
    position: "fixed",
    top: position?.y ?? 200,
    left: position?.x ?? 200,
    zIndex: 150,
    background: "var(--menu-bg)",
    borderRadius: 12,
    padding: 16,
    boxShadow: "var(--menu-shadow)",
    animation: "menuIn 0.12s ease",
  };

  return (
    <div style={style} ref={modalRef} className="custom-reminder-modal">
      <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-secondary)", marginBottom: 12, textTransform: "uppercase", letterSpacing: 0.3 }}>
        Custom reminder
      </div>
      <div style={{ marginBottom: 12 }}>
        <DatePicker
          selected={selectedDate}
          onChange={setSelectedDate}
          dateFormat="yyyy-MM-dd"
          minDate={new Date()}
          inline
        />
      </div>
      <div style={{ marginBottom: 14 }}>
        <label style={{ fontSize: 11, color: "var(--text-secondary)", display: "block", marginBottom: 6, textAlign: "center", textTransform: "uppercase", letterSpacing: 0.3 }}>Time</label>
        <TimeInput hours={hours} minutes={minutes} onChangeHours={setHours} onChangeMinutes={setMinutes} />
      </div>
      <div style={{ display: "flex", gap: 8 }}>
        <button className="dp-btn" tabIndex={3} onClick={onClose} style={{ flex: 1, justifyContent: "center", padding: "10px 0" }}>Cancel</button>
        <button className="dp-btn primary" tabIndex={4} onClick={handleConfirm} style={{ flex: 1, justifyContent: "center", padding: "10px 0" }}>Set reminder</button>
      </div>
    </div>
  );
}
